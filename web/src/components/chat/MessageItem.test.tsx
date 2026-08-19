// MessageItem 渲染测试。

import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { MessageItem } from './MessageItem'
import type { AssistantItem } from '@/lib/chat'

function makeAssistant(overrides: Partial<AssistantItem> = {}): AssistantItem {
  return {
    type: 'assistant',
    id: 'a1',
    text: '',
    thinking: '',
    blocks: [],
    streaming: false,
    stopReason: 'stop',
    ...overrides,
  }
}

describe('MessageItem assistant', () => {
  it('thinking 默认折叠（显示首行摘要），点击展开显示完整内容', async () => {
    const user = userEvent.setup()
    const item = makeAssistant({
      thinking: '第一行推理\n第二行推理',
    })
    render(<MessageItem item={item} />)

    // 折叠态：胶囊标题 + 首行摘要，其余行不可见
    expect(screen.getByText('Think')).toBeInTheDocument()
    expect(screen.getByText('第一行推理')).toBeInTheDocument()
    expect(screen.queryByText(/第二行推理/)).not.toBeInTheDocument()

    // 点击思考胶囊展开（按名称定位，避免命中底部复制按钮）
    await user.click(screen.getByRole('button', { name: /Think/ }))

    // 展开态：完整推理内容可见（第二行仅存在于展开区）
    expect(screen.getByText(/第二行推理/)).toBeInTheDocument()
  })

  it('流式 thinking 显示思考中状态', () => {
    const item = makeAssistant({ thinking: '正在想', streaming: true })
    render(<MessageItem item={item} />)

    expect(screen.getByText('思考中…')).toBeInTheDocument()
  })

  it('空输出时不渲染正文与复制按钮', () => {
    const { rerender } = render(<MessageItem item={makeAssistant({ text: '回复内容' })} />)
    expect(screen.getByRole('button', { name: '复制回复' })).toBeInTheDocument()

    // 空输出：无正文气泡、无复制按钮（组件整体返回 null）
    rerender(<MessageItem item={makeAssistant({ text: '' })} />)
    expect(screen.queryByRole('button', { name: '复制回复' })).not.toBeInTheDocument()
    expect(screen.queryByText('回复内容')).not.toBeInTheDocument()
  })
})
