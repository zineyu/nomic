// 设置页测试：快照渲染（providers / 模型覆盖 / 标量）、编辑对话框写路径
// （补丁三态：留空不改动 / 清除置 null）、删除与错误展示。
//
// api 模块整体 mock：useSettings 走 api.settings/connect/subscribe，
// 写操作断言调用参数（WS 事件负载）。

import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { SettingsSnapshot } from '@/lib/types'

const mocks = vi.hoisted(() => ({
  settings: vi.fn(),
  upsertProvider: vi.fn(),
  deleteProvider: vi.fn(),
  upsertModelSpec: vi.fn(),
  deleteModelSpec: vi.fn(),
  setSetting: vi.fn(),
  unsetSetting: vi.fn(),
  connect: vi.fn().mockResolvedValue(undefined),
  subscribe: vi.fn(() => () => {}),
}))

vi.mock('@/lib/api', () => ({
  api: {
    connect: mocks.connect,
    subscribe: mocks.subscribe,
    settings: mocks.settings,
    upsertProvider: mocks.upsertProvider,
    deleteProvider: mocks.deleteProvider,
    upsertModelSpec: mocks.upsertModelSpec,
    deleteModelSpec: mocks.deleteModelSpec,
    setSetting: mocks.setSetting,
    unsetSetting: mocks.unsetSetting,
  },
}))

import { SettingsView } from './SettingsView'

function snapshot(overrides: Partial<SettingsSnapshot> = {}): SettingsSnapshot {
  return {
    providers: [
      {
        name: 'anthropic',
        api: null,
        base_url: 'https://api.anthropic.com',
        has_api_key: true,
        updated_at: 1,
      },
    ],
    model_specs: [
      {
        provider: 'anthropic',
        model_id: 'claude-sonnet-4-5',
        name: null,
        reasoning: true,
        vision: null,
        context_window: 200000,
        max_tokens: null,
        cost_input: null,
        cost_output: null,
        cost_cache_read: null,
        cost_cache_write: null,
        updated_at: 1,
      },
    ],
    // 服务端仍下发全部已知键（含已有取值），web 端只渲染白名单内的键
    settings: { temperature: 0.7, api_key: '已设置', append_system: '保持简洁' },
    scalar_keys: [
      'base_url',
      'api_key',
      'temperature',
      'max_tokens',
      'append_system',
      'prompts',
      'compaction.enabled',
      'compaction.reserve_tokens',
      'compaction.keep_recent_tokens',
      'model_aliases',
    ],
    ...overrides,
  }
}

describe('SettingsView', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.upsertProvider.mockResolvedValue({})
    mocks.setSetting.mockResolvedValue({})
    mocks.unsetSetting.mockResolvedValue({})
  })

  it('渲染快照：provider / 模型覆盖 / 标量分段', async () => {
    mocks.settings.mockResolvedValue(snapshot())
    render(<SettingsView />)

    await screen.findByText('anthropic')
    expect(screen.getByText('anthropic/claude-sonnet-4-5')).toBeInTheDocument()
    // 列表行直接展示关键信息：provider 摘要与覆盖摘要
    expect(
      screen.getByText(/按名推断 · https:\/\/api\.anthropic\.com · api_key 已设置/),
    ).toBeInTheDocument()
    expect(screen.getByText(/推理 · 上下文 200k/)).toBeInTheDocument()
    // 白名单内的标量键渲染，被移除的键（走默认值）不渲染
    expect(screen.getByText('追加系统提示词')).toBeInTheDocument()
    expect(screen.queryByText('采样温度')).not.toBeInTheDocument()
    expect(screen.queryByText('全局 base_url')).not.toBeInTheDocument()
    expect(screen.queryByText('压缩预留 tokens')).not.toBeInTheDocument()
  })

  it('空快照展示引导文案', async () => {
    mocks.settings.mockResolvedValue(snapshot({ providers: [], model_specs: [], settings: {} }))
    render(<SettingsView />)

    await screen.findByText(/还没有 provider 定义/)
    expect(screen.getByText(/没有模型覆盖/)).toBeInTheDocument()
  })

  it('编辑 provider：api_key 留空不改动，勾选清除置 null', async () => {
    mocks.settings.mockResolvedValue(snapshot())
    render(<SettingsView />)

    await screen.findByText('anthropic')
    await userEvent.click(screen.getByRole('button', { name: '编辑 anthropic' }))
    // 留空 api_key 直接保存：patch 不含 api_key 字段
    await userEvent.click(screen.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(mocks.upsertProvider).toHaveBeenCalledOnce())
    expect(mocks.upsertProvider).toHaveBeenCalledWith('anthropic', {
      api: null,
      base_url: 'https://api.anthropic.com',
    })

    // 勾选清除：patch.api_key = null
    await userEvent.click(screen.getByRole('button', { name: '编辑 anthropic' }))
    await userEvent.click(screen.getByRole('checkbox', { name: /清除已设置的 api_key/ }))
    await userEvent.click(screen.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(mocks.upsertProvider).toHaveBeenCalledTimes(2))
    expect(mocks.upsertProvider).toHaveBeenLastCalledWith('anthropic', {
      api: null,
      base_url: 'https://api.anthropic.com',
      api_key: null,
    })
  })

  it('行内编辑字符串标量：Enter 提交', async () => {
    mocks.settings.mockResolvedValue(snapshot())
    render(<SettingsView />)

    await screen.findByText('追加系统提示词')
    const input = screen.getByLabelText('append_system', { exact: true })
    await userEvent.clear(input)
    await userEvent.type(input, '回答用中文')
    await userEvent.keyboard('{Enter}')
    await waitFor(() => expect(mocks.setSetting).toHaveBeenCalledWith('append_system', '回答用中文'))
  })

  it('行内编辑失败：就地展示错误并还原为当前值', async () => {
    mocks.setSetting.mockRejectedValue(new Error('取值类型非法'))
    mocks.settings.mockResolvedValue(snapshot())
    render(<SettingsView />)

    await screen.findByText('追加系统提示词')
    const input = screen.getByLabelText('append_system', { exact: true })
    await userEvent.clear(input)
    await userEvent.type(input, '新值')
    await userEvent.keyboard('{Enter}')
    await screen.findByRole('alert')
    expect(screen.getByRole('alert')).toHaveTextContent('取值类型非法')
    await waitFor(() => expect(input).toHaveValue('保持简洁'))
  })

  it('布尔标量开关即点即存', async () => {
    mocks.settings.mockResolvedValue(snapshot())
    render(<SettingsView />)

    await screen.findByText('自动压缩开关')
    await userEvent.click(screen.getByRole('switch', { name: 'compaction.enabled' }))
    await waitFor(() =>
      expect(mocks.setSetting).toHaveBeenCalledWith('compaction.enabled', true),
    )
  })

  it('JSON 标量走对话框：留空禁用保存，解析后写入', async () => {
    mocks.settings.mockResolvedValue(snapshot())
    render(<SettingsView />)

    await screen.findByText('模型别名表')
    await userEvent.click(screen.getByRole('button', { name: '编辑 model_aliases' }))
    const input = screen.getByLabelText('model_aliases', { exact: true })
    expect(screen.getByRole('button', { name: '保存' })).toBeDisabled()
    await userEvent.type(input, '{{"smart": "anthropic/claude-sonnet-4-5"}')
    await userEvent.click(screen.getByRole('button', { name: '保存' }))
    await waitFor(() =>
      expect(mocks.setSetting).toHaveBeenCalledWith('model_aliases', {
        smart: 'anthropic/claude-sonnet-4-5',
      }),
    )
  })

  it('加载失败展示错误而非空白', async () => {
    mocks.settings.mockRejectedValue(new Error('库不可用'))
    render(<SettingsView />)
    await screen.findByRole('alert')
    expect(screen.getByRole('alert')).toHaveTextContent('库不可用')
  })
})
