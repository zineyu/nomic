// MessageList 渲染测试。

import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { MessageList } from './MessageList'
import type { ChatItem, ToolItem } from '@/lib/chat'

function makeTool(id: string): ToolItem {
  return {
    type: 'tool',
    id,
    toolCallId: id,
    name: 'bash',
    args: { command: 'ls' },
    status: 'done',
    resultPreview: 'ok',
    isError: false,
  }
}

/** 消息列表的行容器（flex 列，行间距由它控制）。 */
function rowsOf(container: HTMLElement): HTMLCollection {
  return container.querySelector('.flex.flex-col.gap-4')!.children
}

describe('MessageList', () => {
  it('跳过渲染为空的 assistant 消息，避免工具卡片间出现双倍间距', () => {
    // 真实事件流中每次工具调用前都有一条 assistant 消息；
    // 无正文/思考的 tool_use 消息渲染为空，不应再占一行
    const items: ChatItem[] = [
      makeTool('t1'),
      {
        type: 'assistant',
        id: 'a1',
        text: '',
        thinking: '',
        blocks: [],
        streaming: false,
        stopReason: 'tool_use',
      },
      makeTool('t2'),
    ]
    const { container } = render(<MessageList items={items} />)

    expect(rowsOf(container)).toHaveLength(2)
  })

  it('有思考内容的 assistant 消息正常占位（Think 胶囊可见）', () => {
    const items: ChatItem[] = [
      makeTool('t1'),
      {
        type: 'assistant',
        id: 'a1',
        text: '',
        thinking: '先看一下目录结构',
        blocks: [],
        streaming: false,
        stopReason: 'tool_use',
      },
      makeTool('t2'),
    ]
    const { container } = render(<MessageList items={items} />)

    expect(rowsOf(container)).toHaveLength(3)
    expect(screen.getByText('Think')).toBeInTheDocument()
  })
})
