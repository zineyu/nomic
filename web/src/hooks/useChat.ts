// useChat：聊天状态的单一入口——挂载时取快照 + 订阅 SSE，事件增量合并到
// 消息项列表；对外暴露 send / stop / newSession / resumeSession / answerQuestion。
// 会话列表在此集中管理（侧栏只做展示），并在 run_finished 时刷新以捕获标题。

import { useCallback, useEffect, useState } from 'react'

import { api, createStreamClient } from '@/lib/api'
import { applyServerEvent, messagesToItems, type ChatItem } from '@/lib/chat'
import type {
  AskUserAnswer,
  AskUserQuestion,
  Model,
  ServerEvent,
  SessionSummary,
} from '@/lib/types'

export interface QuestionState {
  id: string
  question: AskUserQuestion
}

export interface ChatState {
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
}

const initialState: ChatState = {
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
}

export function useChat() {
  const [state, setState] = useState<ChatState>(initialState)

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
        question:
          snap.pending_question ?? (prev.question?.id ? prev.question : null),
        error: null,
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

  useEffect(() => {
    void refreshSnapshot()
    void refreshSessions()
    const client = createStreamClient()
    const disconnect = client.connect((event) => {
      if (event.type === 'refresh') {
        // 落后于事件流：重新拉取快照补齐
        void refreshSnapshot()
        void refreshSessions()
      } else {
        applyEvent(event)
        // 首条消息后标题会变化，run 结束刷新会话列表
        if (event.type === 'run_finished') void refreshSessions()
      }
    })
    return disconnect
  }, [applyEvent, refreshSnapshot, refreshSessions])

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
    try {
      await api.cancel()
    } catch (error) {
      setState((prev) => ({
        ...prev,
        error: error instanceof Error ? error.message : String(error),
      }))
    }
  }, [])

  const newSession = useCallback(async () => {
    try {
      await api.createSession()
      await refreshSnapshot()
      await refreshSessions()
    } catch (error) {
      setState((prev) => ({
        ...prev,
        error: error instanceof Error ? error.message : String(error),
      }))
    }
  }, [refreshSnapshot, refreshSessions])

  const resumeSession = useCallback(
    async (id: string) => {
      try {
        await api.resumeSession(id)
        await refreshSnapshot()
        await refreshSessions()
      } catch (error) {
        setState((prev) => ({
          ...prev,
          error: error instanceof Error ? error.message : String(error),
        }))
      }
    },
    [refreshSnapshot, refreshSessions],
  )

  const answerQuestion = useCallback(async (id: string, answer: AskUserAnswer) => {
    try {
      await api.answerQuestion(id, answer)
      setState((prev) => ({ ...prev, question: null }))
    } catch (error) {
      setState((prev) => ({
        ...prev,
        error: error instanceof Error ? error.message : String(error),
      }))
    }
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
    answerQuestion,
    refreshSnapshot,
    dismissError,
  }
}

export type UseChat = ReturnType<typeof useChat>
