// ChatInput 测试：Enter 发送、Shift+Enter 换行、运行中切换停止。

import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { ChatInput } from './ChatInput'

describe('ChatInput', () => {
  it('Enter 发送并清空输入', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    render(<ChatInput running={false} queued={0} onSend={onSend} onStop={vi.fn()} />)
    const textarea = screen.getByPlaceholderText(/给 nomic 发消息/)
    await user.type(textarea, 'hello')
    await user.keyboard('{Enter}')
    expect(onSend).toHaveBeenCalledWith('hello')
    expect(textarea).toHaveValue('')
  })

  it('Shift+Enter 不发送（换行）', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    render(<ChatInput running={false} queued={0} onSend={onSend} onStop={vi.fn()} />)
    const textarea = screen.getByPlaceholderText(/给 nomic 发消息/)
    await user.type(textarea, 'a')
    await user.keyboard('{Shift>}{Enter}{/Shift}')
    expect(onSend).not.toHaveBeenCalled()
    // Shift+Enter 走默认行为：插入换行，不发送
    expect(textarea).toHaveValue('a\n')
  })

  it('空输入不发送', async () => {
    const user = userEvent.setup()
    const onSend = vi.fn()
    render(<ChatInput running={false} queued={0} onSend={onSend} onStop={vi.fn()} />)
    await user.click(screen.getByRole('button'))
    expect(onSend).not.toHaveBeenCalled()
  })

  it('运行中显示停止按钮', () => {
    const onStop = vi.fn()
    render(<ChatInput running queued={1} onSend={vi.fn()} onStop={onStop} />)
    expect(screen.getByTitle(/停止当前运行/)).toBeInTheDocument()
    expect(screen.getByText(/已排队 1 条/)).toBeInTheDocument()
  })
})
