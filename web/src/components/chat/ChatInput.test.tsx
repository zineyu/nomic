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