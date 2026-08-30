// 设置页测试：快照渲染（providers / 模型规格 / 标量）、编辑对话框写路径
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
    settings: { temperature: 0.7, api_key: '已设置' },
    scalar_keys: ['base_url', 'api_key', 'temperature', 'model_aliases'],
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

  it('渲染快照：provider 脱敏、规格覆盖摘要、标量当前值', async () => {
    mocks.settings.mockResolvedValue(snapshot())
    render(<SettingsView />)

    await screen.findByText('anthropic')
    // api_key 脱敏：只显示是否已设置，不明文
    expect(screen.getByText(/api_key 已设置/)).toBeInTheDocument()
    expect(screen.getByText('anthropic/claude-sonnet-4-5')).toBeInTheDocument()
    expect(screen.getByText(/context=200000/)).toBeInTheDocument()
    expect(screen.getByText('0.7')).toBeInTheDocument()
  })

  it('空快照展示引导文案', async () => {
    mocks.settings.mockResolvedValue(snapshot({ providers: [], model_specs: [], settings: {} }))
    render(<SettingsView />)

    await screen.findByText(/还没有 provider 定义/)
    expect(screen.getByText(/没有规格覆盖/)).toBeInTheDocument()
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

  it('编辑标量：数字解析后写入；写失败就地展示错误', async () => {
    mocks.settings.mockResolvedValue(snapshot())
    render(<SettingsView />)

    await screen.findByText('0.7')
    await userEvent.click(screen.getByRole('button', { name: '编辑 temperature' }))
    const input = screen.getByLabelText('temperature', { exact: true })
    await userEvent.clear(input)
    await userEvent.type(input, '0.2')
    await userEvent.click(screen.getByRole('button', { name: '保存' }))
    await waitFor(() => expect(mocks.setSetting).toHaveBeenCalledWith('temperature', 0.2))

    mocks.setSetting.mockRejectedValueOnce(new Error('取值类型非法'))
    await userEvent.click(screen.getByRole('button', { name: '编辑 temperature' }))
    await userEvent.click(screen.getByRole('button', { name: '保存' }))
    await screen.findByRole('alert')
    expect(screen.getByRole('alert')).toHaveTextContent('取值类型非法')
  })

  it('加载失败展示错误而非空白', async () => {
    mocks.settings.mockRejectedValue(new Error('库不可用'))
    render(<SettingsView />)
    await screen.findByRole('alert')
    expect(screen.getByRole('alert')).toHaveTextContent('库不可用')
  })
})
