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

    await user.click(screen.getByRole('button'))

    // 展开态：完整推理内容可见（第二行仅存在于展开区）
    expect(screen.getByText(/第二行推理/)).toBeInTheDocument()
  })

  it('流式 thinking 显示思考中状态', () => {
    const item = makeAssistant({ thinking: '正在想', streaming: true })
    render(<MessageItem item={item} />)

    expect(screen.getByText('思考中…')).toBeInTheDocument()
  })
})
