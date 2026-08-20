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

/** 服务端推送事件（所有事件携带 session_id 供前端路由到对应 session） */
export type ServerEvent =
  // ── 生命周期事件（agent 运行驱动）
  | { type: 'agent'; session_id: string; event: AgentEvent }
  | { type: 'question'; session_id: string; id: string; question: AskUserQuestion }
  | { type: 'question_cancelled'; session_id: string; id: string }
  | { type: 'run_started'; session_id: string }
  | { type: 'run_finished'; session_id: string }
  | { type: 'error'; session_id?: string; request_id?: string; message: string }
  | { type: 'refresh' }
  // ── 查询响应事件（携带 request_id）
  | { type: 'state_snapshot'; session_id: string; request_id: string; snapshot: SnapshotView }
  | { type: 'models_list'; request_id: string; candidates: ModelChoice[] }
  | { type: 'sessions_list'; request_id: string; sessions: SessionSummary[] }
  | { type: 'workspaces_list'; request_id: string; workspaces: WorkspaceSummary[] }
  // ── 命令 ack 事件
  | { type: 'prompt_ack'; session_id: string; queued: boolean }
  | { type: 'cancel_ack'; session_id: string }
  | { type: 'answer_ack'; session_id: string }
  | { type: 'switch_model_ack'; session_id: string; choice: ModelChoice }
  | { type: 'session_created'; id: string; title: string | null }
  | { type: 'workspace_created'; request_id: string; id: string; path: string }

/** 客户端发送给服务端的事件（纯事件驱动，无 REST） */
export type ClientEvent =
  // ── 查询类（携带 request_id，响应也带同一 request_id）
  | { type: 'get_state'; session_id: string; request_id: string }
  | { type: 'list_models'; request_id: string }
  | { type: 'list_sessions'; request_id: string }
  | { type: 'list_workspaces'; request_id: string }
  // ── 命令类（fire-and-forget，由后续 ServerEvent 驱动状态）
  | { type: 'prompt'; session_id: string; text: string; images?: ImageContent[] }
  | { type: 'cancel'; session_id: string }
  | { type: 'answer_question'; session_id: string; id: string; answers: string[]; custom?: string | null }
  | { type: 'switch_model'; session_id: string; spec: string; reasoning?: string | null }
  | { type: 'create_session'; workspace?: string }
  // ── 查询式命令（携带 request_id，响应/错误事件带同一 request_id）
  | { type: 'create_workspace'; request_id: string; path: string }

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
  workspace_id: string
  workspace: string
  first_message_at: number | null
  last_message_at: number | null
  message_count: number
}

/** workspace 摘要（nomic-session WorkspaceSummary；path 为规范化路径） */
export interface WorkspaceSummary {
  id: string
  path: string
  session_count: number
  last_active_at: number | null
}

/** 会话统计信息（从会话快照返回） */
export interface SessionStats {
  rounds: number
  total_steps: number
  llm_time_ms: number
  tool_time_ms: number
  avg_first_token_ms: number
  output_token_rate: number
  cache_hit_ratio: number
  input_tokens: number
  output_tokens: number
  subagent_count: number
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
  workspace: string
  /** 会话统计信息 */
  rounds: number
  total_steps: number
  llm_time_ms: number
  tool_time_ms: number
  avg_first_token_ms: number
  output_token_rate: number
  cache_hit_ratio: number
  input_tokens: number
  output_tokens: number
  subagent_count: number
}

/** WebSocket 会话快照响应（与 StateResponse 同构） */
export type SnapshotView = StateResponse

export interface ModelsResponse {
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
