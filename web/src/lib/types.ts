// 与 Rust 侧 serde JSON 对应的类型定义（字段名保持 snake_case，与
// nomic 的 serde 输出一致）。参见 crates/runtime/nomic-ai/src/types.rs 与
// crates/runtime/nomic-core/src/agent/events.rs。

// ── 消息模型（nomic-ai types）─────────────────────────────────────────────

export type ApiKind = 'anthropic_messages' | 'open_ai_completions'

export type StopReason = 'stop' | 'length' | 'tool_use' | 'error' | 'aborted'

export interface TextContent {
  text: string
  text_signature?: string
}

export interface ThinkingContent {
  thinking: string
  thinking_signature?: string
  redacted?: boolean
}

export interface ImageContent {
  data: string
  mime_type: string
}

export interface ToolCall {
  id: string
  name: string
  arguments: Record<string, unknown>
  thought_signature?: string
}

export interface AssistantContent {
  type: 'text' | 'thinking' | 'tool_call'
  text?: string
  thinking?: string
  redacted?: boolean
  id?: string
  name?: string
  arguments?: Record<string, unknown>
}

export interface Usage {
  input: number
  output: number
  cache_read: number
  cache_write: number
  reasoning?: number
  total_tokens: number
  cost: {
    input: number
    output: number
    cache_read: number
    cache_write: number
    total: number
  }
}

export interface UserMessage {
  role: 'user'
  content: string | { type: 'text'; text: string }[] | { type: 'image'; data: string; mime_type: string }[]
  timestamp: number
}

export interface AssistantMessage {
  role: 'assistant'
  content: AssistantContent[]
  api: ApiKind
  provider: string
  model: string
  response_model?: string
  response_id?: string
  usage: Usage
  stop_reason: StopReason
  error_message?: string
  timestamp: number
}

export interface ToolResultMessage {
  role: 'tool_result'
  tool_call_id: string
  tool_name: string
  content: { type: 'text'; text: string }[] | { type: 'image'; data: string; mime_type: string }[]
  details?: Record<string, unknown>
  is_error: boolean
  timestamp: number
}

export type Message = UserMessage | AssistantMessage | ToolResultMessage

// ── agent 事件（nomic-core events）─────────────────────────────────────────

/** assistant 流式增量（nomic-ai stream::AssistantEvent，外部标签形式） */
export type AssistantEvent =
  | 'Start'
  | { TextStart: { index: number } }
  | { TextDelta: { index: number; delta: string } }
  | { TextEnd: { index: number } }
  | { ThinkingStart: { index: number } }
  | { ThinkingDelta: { index: number; delta: string } }
  | { ThinkingEnd: { index: number } }
  | { ToolCallStart: { index: number } }
  | { ToolCallDelta: { index: number; delta: string } }
  | { ToolCallEnd: { index: number; tool_call: ToolCall } }
  | { Done: { message: AssistantMessage } }
  | { Error: { message: AssistantMessage } }

export interface ToolResult {
  content: { type: 'text'; text: string }[] | { type: 'image'; data: string; mime_type: string }[]
  details?: Record<string, unknown>
  terminate: boolean
}

export interface ToolUpdate {
  content: { type: 'text'; text: string }[] | { type: 'image'; data: string; mime_type: string }[]
  details?: Record<string, unknown>
}

/** agent 生命周期事件（外部标签形式） */
export type AgentEvent =
  | 'AgentStart'
  | { AgentEnd: { messages: Message[]; context_tokens: number } }
  | 'TurnStart'
  | { TurnEnd: { message: AssistantMessage; tool_results: ToolResultMessage[] } }
  | { MessageStart: Message }
  | { MessageUpdate: AssistantEvent }
  | { MessageEnd: { message: Message; context_tokens: number } }
  | { CompactionStart: { tokens_before: number } }
  | {
      CompactionEnd: {
        summary: string
        tokens_before: number
        context_tokens: number
        kept_count: number
        usage: Usage
      }
    }
  | {
      ToolExecutionStart: {
        tool_call_id: string
        tool_name: string
        args: Record<string, unknown>
      }
    }
  | {
      ToolExecutionUpdate: {
        tool_call_id: string
        tool_name: string
        partial: ToolUpdate
      }
    }
  | {
      ToolExecutionEnd: {
        tool_call_id: string
        tool_name: string
        result: ToolResult
        is_error: boolean
      }
    }

// ── web 服务端事件（nomic-cli web::ServerEvent）────────────────────────────

export type QuestionKind = 'single_choice' | 'multiple_choice' | 'fill_in'

export interface AskUserQuestion {
  question: string
  kind: QuestionKind
  options: string[]
}

export interface AskUserAnswer {
  answers: string[]
  custom: string | null
}

export type ServerEvent =
  | { type: 'agent'; event: AgentEvent }
  | { type: 'question'; id: string; question: AskUserQuestion }
  | { type: 'question_cancelled'; id: string }
  | { type: 'run_started' }
  | { type: 'run_finished' }
  | { type: 'error'; message: string }

// ── REST 响应（nomic-cli web::api）────────────────────────────────────────

export interface Model {
  id: string
  name: string
  api: ApiKind
  provider: string
  base_url: string
  reasoning: boolean
  context_window: number
  max_tokens: number
  cost_input: number
  cost_output: number
  cost_cache_read: number
  cost_cache_write: number
}

export interface ModelChoice {
  provider: string
  id: string
  name: string
  context_window: number
  reasoning: boolean
}

export interface SessionSummary {
  id: string
  title: string | null
  cwd: string
  first_message_at: number | null
  last_message_at: number | null
  message_count: number
}

export interface StateResponse {
  messages: Message[]
  model: Model
  reasoning: 'minimal' | 'low' | 'medium' | 'high' | null
  context_tokens: number
  running: boolean
  queued: number
  session: { id: string; title: string | null } | null
  pending_question: { id: string; question: AskUserQuestion } | null
  cwd: string
}

export interface ModelsResponse {
  current: { provider: string; model: string }
  candidates: ModelChoice[]
}

/** 从外部标签形式的枚举提取变体名（serde external tagging） */
export function eventKind(event: Record<string, unknown>): string {
  return Object.keys(event)[0] ?? ''
}

/** 从外部标签形式的枚举提取变体负载 */
export function eventPayload(event: Record<string, unknown>): unknown {
  return event[eventKind(event)] ?? {}
}
