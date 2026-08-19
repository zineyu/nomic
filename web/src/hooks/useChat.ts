// useChat：聊天状态的单一入口——挂载时确定当前 session，取快照 + 订阅该
// session 的 SSE，事件增量合并到消息项列表；对外暴露 send / stop /
// newSession / resumeSession / switchModel / answerQuestion。会话列表在此集中
// 管理（侧栏只做展示），并在 run_finished 时刷新以捕获标题。
//
// 多 session：`sessionId` 是当前会话；切换 / 新建会话即更新 `sessionId`，
// 快照与 SSE 连接随之重建（后台会话在服务端继续运行，互不阻塞）。

import { useCallback, useEffect, useState } from 'react'

import { api, createStreamClient } from '@/lib/api'
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

  // 服务端事件的统一入口（快照刷新与 SSE 共用）
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
      }
    })
  }, [])

  // 快照刷新（挂载 + refresh 事件 + 会话切换后）
  const refreshSnapshot = useCallback(async (id: string) => {
    try {
      const snap = await api.state(id)
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

  // 挂载：确定初始 session（后端默认 → 最近 → 新建）。
  useEffect(() => {
    let cancelled = false
    const boot = async () => {
      let id: string | null = null
      try {
        id = (await api.currentSession()).id
      } catch {
        try {
          const sessions = await api.sessions()
          id = sessions.length > 0 ? (sessions[0].id ?? null) : (await api.createSession()).id
        } catch (error) {
          if (!cancelled) {
            setState((prev) => ({
              ...prev,
              error: error instanceof Error ? error.message : String(error),
            }))
          }
          return
        }
      }
      if (!cancelled && id) {
        setState((prev) => ({ ...prev, sessionId: id }))
        void refreshSessions()
      }
    }
    void boot()
    return () => {
      cancelled = true
    }
  }, [refreshSessions])

  // 会话切换：刷新快照 + 重建 SSE 连接。
  useEffect(() => {
    if (!sessionId) return
    void refreshSnapshot(sessionId)
    const client = createStreamClient()
    const disconnect = client.connect(sessionId, (event) => {
      if (event.type === 'refresh') {
        // 落后于事件流：重新拉取快照补齐
        void refreshSnapshot(sessionId)
        void refreshSessions()
      } else {
        applyEvent(event)
        // 首条消息后标题会变化，run 结束刷新会话列表
        if (event.type === 'run_finished') void refreshSessions()
      }
    })
    return disconnect
  }, [sessionId, applyEvent, refreshSnapshot, refreshSessions])

  const send = useCallback(
    async (text: string) => {
      if (!sessionId) return
      const trimmed = text.trim()
      if (!trimmed) return
      try {
        const result = await api.prompt(sessionId, trimmed)
        if (result.status === 'queued') {
          setState((prev) => ({ ...prev, queued: prev.queued + 1 }))
        }
      } catch (error) {
        setState((prev) => ({
          ...prev,
          error: error instanceof Error ? error.message : String(error),
        }))
      }
    },
    [sessionId],
  )

  const stop = useCallback(async () => {
    if (!sessionId) return
    try {
      await api.cancel(sessionId)
    } catch (error) {
      setState((prev) => ({
        ...prev,
        error: error instanceof Error ? error.message : String(error),
      }))
    }
  }, [sessionId])

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

  const switchModel = useCallback(
    async (spec: string, reasoning?: string) => {
      if (!sessionId) return
      try {
        await api.switchModel(sessionId, spec, reasoning)
        await refreshSnapshot(sessionId)
      } catch (error) {
        setState((prev) => ({
          ...prev,
          error: error instanceof Error ? error.message : String(error),
        }))
      }
    },
    [sessionId, refreshSnapshot],
  )

  const answerQuestion = useCallback(
    async (id: string, answer: AskUserAnswer) => {
      if (!sessionId) return
      try {
        await api.answerQuestion(sessionId, id, answer)
        setState((prev) => ({ ...prev, question: null }))
      } catch (error) {
        setState((prev) => ({
          ...prev,
          error: error instanceof Error ? error.message : String(error),
        }))
      }
    },
    [sessionId],
  )

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
