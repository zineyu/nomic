// ChatInput 测试：Enter 发送、Shift+Enter 换行、运行中切换停止、上下文环形指示器。

import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { ChatInput } from './ChatInput'
import { TooltipProvider } from '@/components/ui/tooltip'

function renderChatInput(props: Partial<Parameters<typeof ChatInput>[0]> = {}) {
  return render(
    <TooltipProvider delayDuration={0}>
      <ChatInput running={false} queued={0} onSend={vi.fn()} onStop={vi.fn()} {...props} />
    </TooltipProvider>,
  )
}

describe('ChatInput', () => {
  it('Enter 发送并清空输入', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    renderChatInput({ onSend })
    const textarea = screen.getByPlaceholderText(/给智能体发消息/)
    await user.type(textarea, 'hello')
    await user.keyboard('{Enter}')
    expect(onSend).toHaveBeenCalledWith('hello')
    expect(textarea).toHaveValue('')
  })

  it('Shift+Enter 不发送（换行）', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    renderChatInput({ onSend })
    const textarea = screen.getByPlaceholderText(/给智能体发消息/)
    await user.type(textarea, 'a')
    await user.keyboard('{Shift>}{Enter}{/Shift}')
    expect(onSend).not.toHaveBeenCalled()
    // Shift+Enter 走默认行为：插入换行，不发送
    expect(textarea).toHaveValue('a\n')
  })

  it('空输入不发送', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    renderChatInput({ onSend })
    // 点击发送按钮（标题含"发送"）——空输入时按钮禁用，不应触发 onSend
    const sendButton = screen.getByTitle(/发送/)
    expect(sendButton).toBeDisabled()
    await user.click(sendButton)
    expect(onSend).not.toHaveBeenCalled()
  })

  it('运行中显示停止按钮', () => {
    const onStop = vi.fn()
    renderChatInput({ running: true, queued: 1, onStop })
    expect(screen.getByTitle(/停止当前运行/)).toBeInTheDocument()
    expect(screen.getByText(/已排队 1 条/)).toBeInTheDocument()
  })

  it('输入框显示上下文环形指示器，悬停展示详情', async () => {
    const user = userEvent.setup()
    renderChatInput({ contextTokens: 18_432, contextWindow: 262_144 })

    const ring = screen.getByRole('img', { name: /上下文/ })
    expect(ring).toBeInTheDocument()
    expect(ring).toHaveAttribute(
      'aria-label',
      '上下文：18,432 / 262,144 tokens (7%)',
    )

    await user.hover(ring)
    await waitFor(() => {
      expect(screen.getByRole('tooltip')).toHaveTextContent(
        '上下文：18,432 / 262,144 tokens (7%)',
      )
    })
  })
})
// ── 补全弹层（`@` mention / `/` 命令）─────────────────────────────────────

import { api } from '@/lib/api'

vi.mock('@/lib/api', () => ({
  api: {
    skills: vi.fn(),
    files: vi.fn(),
  },
}))

const mockedApi = vi.mocked(api)

describe('ChatInput 补全弹层', () => {
  it('输入 @ 弹出类型候选，Tab 接受后继续键入', async () => {
    const user = userEvent.setup()
    mockedApi.skills.mockResolvedValue([{ name: 'rust-review', description: '审查 unsafe' }])
    renderChatInput()
    const textarea = screen.getByPlaceholderText(/给智能体发消息/)

    await user.type(textarea, '@')
    expect(await screen.findByText('@skill:')).toBeInTheDocument()
    expect(screen.getByText('@file:')).toBeInTheDocument()

    // Tab 接受类型候选：写入 @skill: 并弹出 skill 名候选
    await user.keyboard('{Tab}')
    expect(textarea).toHaveValue('@skill:')
    expect(await screen.findByText('@skill:rust-review')).toBeInTheDocument()
    expect(screen.getByText('审查 unsafe')).toBeInTheDocument()
  })

  it('@skill: 候选按名称前缀过滤，Enter 接受补尾随空格', async () => {
    const user = userEvent.setup()
    mockedApi.skills.mockResolvedValue([
      { name: 'rust-review', description: '审查 unsafe' },
      { name: 'rust-doc', description: '文档' },
    ])
    renderChatInput()
    const textarea = screen.getByPlaceholderText(/给智能体发消息/)

    await user.type(textarea, '看看 @skill:rust-r')
    const candidate = await screen.findByText('@skill:rust-review')
    expect(screen.queryByText('@skill:rust-doc')).not.toBeInTheDocument()

    // Enter 接受候选而非发送；片段被替换并补尾随空格
    await user.keyboard('{Enter}')
    expect(textarea).toHaveValue('看看 @skill:rust-review ')
    expect(candidate).not.toBeInTheDocument()
  })

  it('@file: 候选经服务端按 session workspace 前缀查询', async () => {
    const user = userEvent.setup()
    mockedApi.files.mockResolvedValue(['src/main.rs', 'src/mod.rs'])
    renderChatInput({ sessionId: 's1' })
    const textarea = screen.getByPlaceholderText(/给智能体发消息/)

    await user.type(textarea, '@file:src/m')
    // 防抖后经 api.files 查询（前缀原样透传）
    await waitFor(() => expect(mockedApi.files).toHaveBeenCalledWith('s1', 'src/m'))
    expect(await screen.findByText('@file:src/main.rs')).toBeInTheDocument()

    // 无 session 时文件补全不可用
    mockedApi.files.mockClear()
    renderChatInput()
    const other = screen.getAllByPlaceholderText(/给智能体发消息/)[1]
    await user.type(other, '@file:src/')
    await new Promise((resolve) => setTimeout(resolve, 200))
    expect(mockedApi.files).not.toHaveBeenCalled()
  })

  it('/ 开头弹出命令候选，方向键选择、Enter 接受', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    renderChatInput({ onSend })
    const textarea = screen.getByPlaceholderText(/给智能体发消息/)

    await user.type(textarea, '/c')
    expect(await screen.findByText('/compact [聚焦指令]')).toBeInTheDocument()
    expect(screen.getByText('/continue')).toBeInTheDocument()

    // ↓ 选中 /continue，Enter 接受（不发送）
    await user.keyboard('{ArrowDown}{Enter}')
    expect(textarea).toHaveValue('/continue ')
    expect(onSend).not.toHaveBeenCalled()

    // 弹层关闭后 Enter 正常发送（trim 后交给服务端解析）
    await user.keyboard('{Enter}')
    expect(onSend).toHaveBeenCalledWith('/continue')
  })

  it('Esc 关闭弹层，Enter 原样发送', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    renderChatInput({ onSend })
    const textarea = screen.getByPlaceholderText(/给智能体发消息/)

    await user.type(textarea, '/com')
    expect(await screen.findByText('/compact [聚焦指令]')).toBeInTheDocument()
    await user.keyboard('{Escape}')
    expect(screen.queryByText('/compact [聚焦指令]')).not.toBeInTheDocument()

    await user.keyboard('{Enter}')
    expect(onSend).toHaveBeenCalledWith('/com')
  })
})
