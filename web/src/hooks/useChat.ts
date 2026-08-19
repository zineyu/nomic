// useChat：聊天状态的单一入口——挂载时确定当前 session，通过 WebSocket
// 事件流获取快照 + 接收增量事件，事件驱动合并到消息项列表；
// 对外暴露 send / stop / newSession / resumeSession / switchModel / answerQuestion。
//
// 多 session：`sessionId` 是当前会话；切换 / 新建会话即更新 `sessionId`，
// WebSocket 连接随之重建（后台会话在服务端继续运行，互不阻塞）。
//
// 纯事件驱动：所有前端↔后端通信通过 WebSocket 双向事件流，无 REST。

import { useCallback, useEffect, useRef, useState } from 'react'

import { api } from '@/lib/api'
import { applyServerEvent, messagesToItems, type ChatItem } from '@/lib/chat'
import type {
  AskUserAnswer,
  AskUserQuestion,
  Model,
  ServerEvent,
  SessionStats,
  SessionSummary,
} from '@/lib/types'

export interface QuestionState {
  id: string
  question: AskUserQuestion
}

export interface ChatState {
  /** 当前会话 id（切换 / 新建时更新，驱动快照与流重建） */
  sessionId: string | null
  items: ChatItem[]
  sessions: SessionSummary[]
  running: boolean
  queued: number
  model: Model | null
  reasoning: string | null
  contextTokens: number
  session: { id: string; title: string | null } | null
  cwd: string
  question: QuestionState | null
  error: string | null
  stats: SessionStats
}

const defaultStats: SessionStats = {
  rounds: 0,
  total_steps: 0,
  llm_time_ms: 0,
  tool_time_ms: 0,
  avg_first_token_ms: 0,
  output_token_rate: 0,
  cache_hit_ratio: 0,
  input_tokens: 0,
  output_tokens: 0,
  subagent_count: 0,
}

const initialState: ChatState = {
  sessionId: null,
  items: [],
  sessions: [],
  running: false,
  queued: 0,
  model: null,
  reasoning: null,
  contextTokens: 0,
  session: null,
  cwd: '',
  question: null,
  error: null,
  stats: defaultStats,
}

export function useChat() {
  const [state, setState] = useState<ChatState>(initialState)
  const sessionId = state.sessionId
  const sessionIdRef = useRef<string | null>(sessionId)
  sessionIdRef.current = sessionId

  // 服务端事件的统一入口（快照刷新与 WebSocket 共用）
  const applyEvent = useCallback((event: ServerEvent) => {
    setState((prev) => {
      const items = applyServerEvent(prev.items, event)
      switch (event.type) {
        case 'run_started':
          return { ...prev, items, running: true, queued: 0 }
        case 'run_finished':
          return { ...prev, items, running: false, queued: 0 }
        case 'question':
          return { ...prev, items, question: { id: event.id, question: event.question } }
        case 'question_cancelled':
          return {
            ...prev,
            items,
            question: prev.question?.id === event.id ? null : prev.question,
          }
        case 'error':
          // agent loop 整体失败时无 run_finished，这里兜底复位运行状态
          return { ...prev, items, running: false, error: event.message }
        case 'agent':
          return { ...prev, items }
        default:
          return prev
      }
    })
  }, [])

  // 快照刷新（挂载 + refresh 事件 + 会话切换后）
  const refreshSnapshot = useCallback(async () => {
    try {
      const snap = await api.state()
      setState((prev) => ({
        ...prev,
        items: messagesToItems(snap.messages),
        model: snap.model,
        reasoning: snap.reasoning,
        contextTokens: snap.context_tokens,
        running: snap.running,
        queued: snap.queued,
        session: snap.session,
        cwd: snap.cwd,
        question: snap.pending_question ?? null,
        error: null,
        stats: {
          rounds: snap.rounds ?? 0,
          total_steps: snap.total_steps ?? 0,
          llm_time_ms: snap.llm_time_ms ?? 0,
          tool_time_ms: snap.tool_time_ms ?? 0,
          avg_first_token_ms: snap.avg_first_token_ms ?? 0,
          output_token_rate: snap.output_token_rate ?? 0,
          cache_hit_ratio: snap.cache_hit_ratio ?? 0,
          input_tokens: snap.input_tokens ?? 0,
          output_tokens: snap.output_tokens ?? 0,
          subagent_count: snap.subagent_count ?? 0,
        },
      }))
    } catch (error) {
      setState((prev) => ({
        ...prev,
        error: error instanceof Error ? error.message : String(error),
      }))
    }
  }, [])

  const refreshSessions = useCallback(async () => {
    try {
      const sessions = await api.sessions()
      setState((prev) => ({ ...prev, sessions }))
    } catch {
      // 会话列表加载失败不阻塞主流程（侧栏显示空态）
    }
  }, [])

  // 事件订阅（mount 时注册一次，整个生命周期有效）
  useEffect(() => {
    return api.subscribe((event) => {
      if (event.type === 'refresh') {
        void refreshSnapshot()
        void refreshSessions()
      } else {
        applyEvent(event)
        // 首条消息后标题会变化，run 结束刷新会话列表
        if (event.type === 'run_finished') void refreshSessions()
      }
    })
  }, [applyEvent, refreshSnapshot, refreshSessions])

  // 挂载：确定初始 session（连接 WebSocket → 请求快照 → 初始化）。
  useEffect(() => {
    let cancelled = false
    const boot = async () => {
      // 连接到 session（`/ws/default` 路径由服务端解析为默认 session）
      await api.setSession('default')
      if (cancelled) return
      // 请求快照（包含 session id、消息历史、模型等）
      await refreshSnapshot()
      void refreshSessions()
    }
    void boot().catch((error) => {
      if (!cancelled) {
        setState((prev) => ({
          ...prev,
          error: error instanceof Error ? error.message : String(error),
        }))
      }
    })
    return () => {
      cancelled = true
    }
  }, [refreshSnapshot, refreshSessions])

  // 会话切换：重建 WebSocket 连接 + 刷新快照。
  useEffect(() => {
    if (!sessionId || sessionId === 'default') return
    void api.setSession(sessionId).then(() => void refreshSnapshot())
  }, [sessionId, refreshSnapshot])

  const send = useCallback(async (text: string) => {
    const trimmed = text.trim()
    if (!trimmed) return
    try {
      const result = await api.prompt(trimmed)
      if (result.status === 'queued') {
        setState((prev) => ({ ...prev, queued: prev.queued + 1 }))
      }
    } catch (error) {
      setState((prev) => ({
        ...prev,
        error: error instanceof Error ? error.message : String(error),
      }))
    }
  }, [])

  const stop = useCallback(async () => {
    api.cancel()
  }, [])

  const newSession = useCallback(async () => {
    try {
      const { id } = await api.createSession()
      setState((prev) => ({ ...prev, sessionId: id }))
      await refreshSessions()
    } catch (error) {
      setState((prev) => ({
        ...prev,
        error: error instanceof Error ? error.message : String(error),
      }))
    }
  }, [refreshSessions])

  const resumeSession = useCallback(async (id: string) => {
    // 仅切换当前会话：快照与流连接由 sessionId effect 重建
    setState((prev) => ({ ...prev, sessionId: id }))
  }, [])

  const switchModel = useCallback(async (spec: string, reasoning?: string) => {
    api.switchModel(spec, reasoning)
    // 等待 switch_model_ack 后刷新快照（由事件流驱动）
    // 快照刷新由 switch_model_ack 事件触发，或由 run_finished 触发
  }, [])

  const answerQuestion = useCallback(async (id: string, answer: AskUserAnswer) => {
    api.answerQuestion(id, answer)
    setState((prev) => ({ ...prev, question: null }))
  }, [])

  const dismissError = useCallback(() => {
    setState((prev) => ({ ...prev, error: null }))
  }, [])

  return {
    ...state,
    send,
    stop,
    newSession,
    resumeSession,
    switchModel,
    answerQuestion,
    dismissError,
  }
}

export type UseChat = ReturnType<typeof useChat>
