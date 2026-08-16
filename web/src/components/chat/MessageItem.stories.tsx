// MessageItem stories：user / assistant（markdown、thinking、工具调用块）/
// tool / system 各形态。

import type { Meta, StoryObj } from '@storybook/react'

import { MessageItem } from '@/components/chat/MessageItem'
import type { ChatItem } from '@/lib/chat'

const meta: Meta<typeof MessageItem> = {
  title: 'chat/MessageItem',
  component: MessageItem,
  parameters: { layout: 'padded' },
}

export default meta
type Story = StoryObj<typeof MessageItem>

export const UserMessage: Story = {
  args: {
    item: {
      type: 'user',
      id: '1',
      text: '用 Rust 写一个快速排序，并解释复杂度。',
      images: [],
    } satisfies ChatItem,
  },
}

export const AssistantMarkdown: Story = {
  args: {
    item: {
      type: 'assistant',
      id: '2',
      text: `当然可以。快速排序的平均复杂度是 **O(n log n)**：

\`\`\`rust
fn quicksort(mut v: Vec<i32>) -> Vec<i32> {
    if v.len() <= 1 { return v; }
    let pivot = v.remove(0);
    let (mut left, mut right): (Vec<_>, Vec<_>) = v.into_iter().partition(|&x| x < pivot);
    left = quicksort(left);
    right = quicksort(right);
    [left, vec![pivot], right].concat()
}
\`\`\`

- 最坏情况（已排序输入 + 固定选 pivot）退化为 **O(n²)**
- 原地版本空间复杂度 O(log n)`,
      thinking: '',
      blocks: [],
      streaming: false,
      stopReason: 'stop',
      model: 'openai/gpt-4o',
    } satisfies ChatItem,
  },
}

export const AssistantThinking: Story = {
  args: {
    item: {
      type: 'assistant',
      id: '3',
      text: '结论：这个改动是安全的。',
      thinking:
        '用户问的是并发写入是否安全。需要检查 BEGIN IMMEDIATE 的语义……\n从 ADR 看，WAL 模式下先读后写会触发 SQLITE_BUSY_SNAPSHOT，所以预先取写锁是正确做法。',
      blocks: [],
      streaming: false,
      stopReason: 'stop',
      model: 'anthropic/claude-sonnet-4-5',
    } satisfies ChatItem,
  },
}

export const AssistantToolCalls: Story = {
  args: {
    item: {
      type: 'assistant',
      id: '4',
      text: '让我先看一下项目结构。',
      thinking: '',
      blocks: [
        { type: 'tool_call', id: 'tc1', name: 'find', arguments: { pattern: '*.toml' } },
        { type: 'tool_call', id: 'tc2', name: 'read', arguments: { path: 'Cargo.toml' } },
      ],
      streaming: false,
      stopReason: 'tool_use',
      model: 'openai/gpt-4o',
    } satisfies ChatItem,
  },
}

export const AssistantFailed: Story = {
  args: {
    item: {
      type: 'assistant',
      id: '5',
      text: '',
      thinking: '',
      blocks: [],
      streaming: false,
      stopReason: 'error',
      errorMessage: 'provider 返回 429：rate limit exceeded，重试耗尽',
      model: 'openai/gpt-4o',
    } satisfies ChatItem,
  },
}

export const ToolRunning: Story = {
  args: {
    item: {
      type: 'tool',
      id: '6',
      toolCallId: 't1',
      name: 'bash',
      args: { command: 'cargo nextest run --workspace' },
      status: 'running',
      resultPreview: '',
      isError: false,
    } satisfies ChatItem,
  },
}

export const ToolDone: Story = {
  args: {
    item: {
      type: 'tool',
      id: '7',
      toolCallId: 't2',
      name: 'grep',
      args: { pattern: 'CancellationToken', path: 'crates/runtime' },
      status: 'done',
      resultPreview: 'crates/runtime/nomic-core/src/agent/actor.rs:31:use tokio_util::sync::CancellationToken;',
      isError: false,
    } satisfies ChatItem,
  },
}

export const SystemNotice: Story = {
  args: {
    item: {
      type: 'system',
      id: '8',
      text: '⟳ 压缩上下文（约 120000 tokens）…',
    } satisfies ChatItem,
  },
}
