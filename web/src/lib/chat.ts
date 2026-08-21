// 聊天状态模型：把 agent 事件流（含历史快照）规整为可渲染的消息项列表。
//
// 消息列表是唯一事实源：历史快照（会话快照的 messages）直接渲染；
// 运行中的增量事件驱动「流式项」更新，MessageEnd 定稿后用完整消息替换。
// 工具卡片由 ToolExecution* 事件驱动（live），历史中的 ToolResult 消息
// 直接渲染（resume 后无事件可回放，两者统一展示形态）。
//
// `applyServerEvent` / `applyAgentEvent` 为纯函数，可直接单测。

import type {
  AgentEvent,
  AssistantContent,
  ImageContent,
  Message,
  ServerEvent,
  StopReason,
  ToolResultMessage,
  Usage,
} from './types'
import { eventKind, eventPayload } from './types'

export type ToolStatus = 'running' | 'done' | 'error'

export interface ToolItem {
  type: 'tool'
  id: string
  toolCallId: string
  name: string
  args: Record<string, unknown>
  status: ToolStatus
  /** 结果文本预览（纯文本块拼接） */
  resultPreview: string
  isError: boolean
}

export interface AssistantItem {
  type: 'assistant'
  id: string
  /** 流式累积/最终文本 */
  text: string
  thinking: string
  /** 最终内容块（MessageEnd 后权威） */
  blocks: AssistantContent[]
  streaming: boolean
  stopReason?: StopReason
  errorMessage?: string
  model?: string
  usage?: Usage
}

export interface UserItem {
  type: 'user'
  id: string
  text: string
  images: ImageContent[]
  /** 消息时间戳（毫秒），用于渲染时间元信息 */
  timestamp: number
}

export interface SystemItem {
  type: 'system'
  id: string
  text: string
}

export type ChatItem = UserItem | AssistantItem | ToolItem | SystemItem

let idCounter = 0
function nextId(): string {
  idCounter += 1
  return `item-${Date.now()}-${idCounter}`
}

/** 从消息内容块提取纯文本（用于工具结果预览）。 */
export function contentText(
  content: { type: 'text'; text: string }[] | { type: 'image'; data: string; mime_type: string }[],
): string {
  return content
    .filter((block): block is { type: 'text'; text: string } => block.type === 'text')
    .map((block) => block.text)
    .join('\n')
}

/** 从用户消息内容提取文本与图片。 */
export function userContent(message: { role: 'user'; content: string | { type: 'text' | 'image'; text?: string; data?: string; mime_type?: string }[] }) {
  if (typeof message.content === 'string') {
    return { text: message.content, images: [] as ImageContent[] }
  }
  const text = message.content
    .filter((block) => block.type === 'text')
    .map((block) => block.text ?? '')
    .join('')
  const images = message.content
    .filter((block) => block.type === 'image')
    .map((block) => ({ data: block.data ?? '', mime_type: block.mime_type ?? 'image/png' }))
  return { text, images }
}

/** 历史消息 → 消息项（会话快照与 resume 用）。 */
export function messagesToItems(messages: Message[]): ChatItem[] {
  const items: ChatItem[] = []
  for (const message of messages) {
    switch (message.role) {
      case 'user': {
        const { text, images } = userContent(message)
        items.push({
          type: 'user',
          id: nextId(),
          text,
          images,
          timestamp: message.timestamp,
        })
        break
      }
      case 'assistant': {
        const text = message.content
          .filter((block) => block.type === 'text')
          .map((block) => block.text ?? '')
          .join('')
        const thinking = message.content
          .filter((block) => block.type === 'thinking')
          .map((block) => block.thinking ?? '')
          .join('\n')
        items.push({
          type: 'assistant',
          id: nextId(),
          text,
          thinking,
          blocks: message.content,
          streaming: false,
          stopReason: message.stop_reason,
          errorMessage: message.error_message,
          model: message.model,
          usage: message.usage,
        })
        // 从历史 assistant 消息中的 tool_call 块恢复工具调用参数，
        // 确保 resume 后工具卡片仍能显示参数。
        for (const block of message.content) {
          if (block.type !== 'tool_call') continue
          const toolCallId = block.id ?? ''
          if (findToolIndex(items, toolCallId) >= 0) continue
          items.push({
            type: 'tool',
            id: nextId(),
            toolCallId,
            name: block.name ?? '',
            args: block.arguments ?? {},
            status: 'running',
            resultPreview: '',
            isError: false,
          })
        }
        break
      }
      case 'tool_result': {
        const index = findToolIndex(items, message.tool_call_id)
        if (index >= 0) {
          const existing = items[index]
          if (existing.type === 'tool') {
            items[index] = {
              ...existing,
              status: message.is_error ? 'error' : 'done',
              resultPreview: contentText(message.content),
              isError: message.is_error,
            }
            break
          }
        }
        items.push({
          type: 'tool',
          id: nextId(),
          toolCallId: message.tool_call_id,
          name: message.tool_name,
          args: {},
          status: message.is_error ? 'error' : 'done',
          resultPreview: contentText(message.content),
          isError: message.is_error,
        })
        break
      }
    }
  }
  return items
}

/** 提取 assistant 消息的文本/思考（MessageEnd 定稿用）。 */
export function assistantText(message: { role: 'assistant'; content: AssistantContent[] }): {
  text: string
  thinking: string
} {
  const text = message.content
    .filter((block) => block.type === 'text')
    .map((block) => block.text ?? '')
    .join('')
  const thinking = message.content
    .filter((block) => block.type === 'thinking')
    .map((block) => block.thinking ?? '')
    .join('\n')
  return { text, thinking }
}

/** 当前流式 assistant 项（最后一个 streaming 的 assistant；无则 `None`）。 */
function findStreamingAssistant(items: ChatItem[]): number {
  for (let i = items.length - 1; i >= 0; i -= 1) {
    const item = items[i]
    if (item.type === 'assistant' && item.streaming) return i
  }
  return -1
}

/** 按 tool_call_id 找到工具项（含已定稿的）；无则 `None`。 */
function findToolIndex(items: ChatItem[], toolCallId: string): number {
  return items.findIndex((item) => item.type === 'tool' && item.toolCallId === toolCallId)
}

function appendItem(items: ChatItem[], item: ChatItem): ChatItem[] {
  return [...items, item]
}

/** 应用一个 web 服务端事件（agent 事件 + 运行/提问事件）到消息项列表。 */
export function applyServerEvent(items: ChatItem[], event: ServerEvent): ChatItem[] {
  switch (event.type) {
    case 'agent':
      return applyAgentEvent(items, event.event)
    case 'question':
    case 'question_cancelled':
    case 'run_started':
    case 'run_finished':
    case 'error':
      // 运行/提问状态由 hook 持有（见 useChat），不进入消息项列表
      return items
    default:
      // 响应事件（state_snapshot / models_list 等）与 ack 事件由 hook 处理
      return items
  }
}

/** 从 agent 事件提取权威上下文 token 数（MessageEnd/AgentEnd/CompactionEnd 携带）。 */
export function agentEventContextTokens(event: AgentEvent): number | null {
  const kind = eventKind(event as unknown as Record<string, unknown>)
  const payload = eventPayload(event as unknown as Record<string, unknown>) as {
    context_tokens?: unknown
  }
  switch (kind) {
    case 'MessageEnd':
    case 'AgentEnd':
    case 'CompactionEnd':
      return typeof payload.context_tokens === 'number' ? payload.context_tokens : null
    default:
      return null
  }
}

/** 应用一个 agent 生命周期事件到消息项列表（纯函数）。 */
export function applyAgentEvent(items: ChatItem[], event: AgentEvent): ChatItem[] {
  const kind = eventKind(event as unknown as Record<string, unknown>)
  const payload = eventPayload(event as unknown as Record<string, unknown>)
  switch (kind) {
    case 'AgentStart':
    case 'TurnStart':
    case 'AgentEnd':
    case 'TurnEnd':
      return items

    case 'MessageStart': {
      const message = payload as Message
      switch (message.role) {
        case 'user': {
          const { text, images } = userContent(message)
          return appendItem(items, {
            type: 'user',
            id: nextId(),
            text,
            images,
            timestamp: message.timestamp,
          })
        }
        case 'assistant':
          return appendItem(items, {
            type: 'assistant',
            id: nextId(),
            text: '',
            thinking: '',
            blocks: [],
            streaming: true,
          })
        case 'tool_result':
          return upsertToolResult(items, payload as ToolResultMessage)
      }
      break
    }

    case 'MessageUpdate': {
      const kind = eventKind(payload as Record<string, unknown>)
      const detail = eventPayload(payload as Record<string, unknown>)
      let index = findStreamingAssistant(items)
      let next = items
      if (index < 0 && (kind === 'Start' || kind === 'TextDelta')) {
        next = appendItem(next, {
          type: 'assistant',
          id: nextId(),
          text: '',
          thinking: '',
          blocks: [],
          streaming: true,
        })
        index = next.length - 1
      }
      if (index < 0) return next
      const current = next[index]
      if (current.type !== 'assistant') return next
      if (kind === 'TextDelta') {
        const delta = (detail as { delta: string }).delta
        return next.with(index, { ...current, text: current.text + delta })
      }
      if (kind === 'ThinkingDelta') {
        const delta = (detail as { delta: string }).delta
        return next.with(index, { ...current, thinking: current.thinking + delta })
      }
      return next
    }

    case 'MessageEnd': {
      const message = (payload as { message: Message }).message
      if (message.role === 'assistant') {
        const index = findStreamingAssistant(items)
        const { text, thinking } = assistantText(message)
        const finalized: ChatItem = {
          type: 'assistant',
          id: index >= 0 ? (items[index] as AssistantItem).id : nextId(),
          text,
          thinking,
          blocks: message.content,
          streaming: false,
          stopReason: message.stop_reason,
          errorMessage: message.error_message,
          model: message.model,
          usage: message.usage,
        }
        if (index >= 0) return items.with(index, finalized)
        return appendItem(items, finalized)
      }
      if (message.role === 'tool_result') {
        return upsertToolResult(items, message)
      }
      return items
    }

    case 'ToolExecutionStart': {
      const detail = payload as {
        tool_call_id: string
        tool_name: string
        args: Record<string, unknown>
      }
      const index = findToolIndex(items, detail.tool_call_id)
      if (index >= 0) {
        const existing = items[index]
        if (existing.type === 'tool') {
          return items.with(index, {
            ...existing,
            name: detail.tool_name,
            args: detail.args,
            status: 'running',
          })
        }
      }
      const tool: ChatItem = {
        type: 'tool',
        id: nextId(),
        toolCallId: detail.tool_call_id,
        name: detail.tool_name,
        args: detail.args,
        status: 'running',
        resultPreview: '',
        isError: false,
      }
      return appendItem(items, tool)
    }

    case 'ToolExecutionUpdate': {
      const detail = payload as {
        tool_call_id: string
        partial: { content: { type: 'text'; text: string }[] }
      }
      const index = findToolIndex(items, detail.tool_call_id)
      if (index < 0) return items
      const current = items[index]
      if (current.type !== 'tool') return items
      return items.with(index, {
        ...current,
        resultPreview: contentText(detail.partial.content),
      })
    }

    case 'ToolExecutionEnd': {
      const detail = payload as {
        tool_call_id: string
        result: { content: { type: 'text'; text: string }[] }
        is_error: boolean
      }
      const index = findToolIndex(items, detail.tool_call_id)
      const preview = contentText(detail.result.content)
      if (index < 0) {
        return items
      }
      const current = items[index]
      if (current.type !== 'tool') return items
      return items.with(index, {
        ...current,
        status: detail.is_error ? 'error' : 'done',
        resultPreview: preview,
        isError: detail.is_error,
      })
    }

    case 'CompactionStart': {
      const detail = payload as { tokens_before: number }
      return appendItem(items, {
        type: 'system',
        id: nextId(),
        text: `⟳ 压缩上下文（约 ${detail.tokens_before} tokens）…`,
      })
    }

    case 'CompactionEnd': {
      const detail = payload as {
        summary: string
        tokens_before: number
        context_tokens: number
        kept_count: number
      }
      return appendItem(items, {
        type: 'system',
        id: nextId(),
        text: `✂ 上下文已压缩：约 ${detail.tokens_before} tokens → 摘要 + ${detail.kept_count} 条近期消息`,
      })
    }
  }
  return items
}

/** 工具结果消息 → 工具项（按 tool_call_id 去重 upsert，兼容 live 与 resume）。 */
function upsertToolResult(items: ChatItem[], message: ToolResultMessage): ChatItem[] {
  const index = findToolIndex(items, message.tool_call_id)
  if (index >= 0) {
    const existing = items[index]
    if (existing.type === 'tool') {
      return items.with(index, {
        ...existing,
        name: message.tool_name,
        status: message.is_error ? 'error' : 'done',
        resultPreview: contentText(message.content),
        isError: message.is_error,
      })
    }
  }
  const tool: ChatItem = {
    type: 'tool',
    id: nextId(),
    toolCallId: message.tool_call_id,
    name: message.tool_name,
    args: {},
    status: message.is_error ? 'error' : 'done',
    resultPreview: contentText(message.content),
    isError: message.is_error,
  }
  return appendItem(items, tool)
}
