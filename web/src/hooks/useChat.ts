// useChat：聊天状态的单一入口——WebSocket 连接后自动接收全局事件总线上的所有
// session 事件（每个事件携带 `session_id`），仅当前查看 session 的事件驱动 UI；
// 对外暴露 send / stop / newSession / startSession / resumeSession / switchModel /
// answerQuestion。
//
// 多 session 并行：所有已打开 session 的事件都通过同一连接推送。`sessionId` 为
// 当前查看的 session；切换查看仅影响 UI 展示，后台 session 的事件流不受影响。
//
// 启动页：无默认 workspace/session——挂载时不加载任何 session（`sessionId` 为
// null），前端展示启动页（workspace 选择栏 + 输入框），首条消息经 startSession
// 在选定 workspace 下创建 session 并发送。
//
// 纯事件驱动：所有前端↔后端通信通过 WebSocket 双向事件流，无 REST。

import { useCallback, useEffect, useRef, useState } from 'react'

import { api } from '@/lib/api'
import { agentEventContextTokens, applyServerEvent, messagesToItems, type ChatItem } from '@/lib/chat'
import type {
  AskUserAnswer,
  AskUserQuestion,
  ImageContent,
  Model,
  QueueEntry,
  ServerEvent,
  SessionStats,
  SessionSummary,
  SnapshotView,
  WorkspaceSummary,
} from '@/lib/types'

export interface QuestionState {
  id: string
  question: AskUserQuestion
}

export interface ChatState {
  /** 当前查看的 session id（切换时驱动 UI 刷新，不影响事件接收） */
  sessionId: string | null
  items: ChatItem[]
  sessions: SessionSummary[]
  running: boolean
  /** steering 队列内容（服务端权威：queue_changed 事件与快照驱动） */
  queue: QueueEntry[]
  model: Model | null
  reasoning: string | null
  contextTokens: number
  session: { id: string; title: string | null } | null
  /** 已登记的全部 workspace（含无会话的；store 不可用时为空，分组退化为纯会话） */
  workspaces: WorkspaceSummary[]
  question: QuestionState | null
  error: string | null
  /** 进行中的目标原文（/goal <目标> 启动；目标驱动运行徽标用） */
  goal: string | null
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
  queue: [],
  model: null,
  reasoning: null,
  contextTokens: 0,
  session: null,
  workspaces: [],
  question: null,
  error: null,
  goal: null,
  stats: defaultStats,
}

export function useChat() {
  const [state, setState] = useState<ChatState>(initialState)
  const sessionId = state.sessionId
  const sessionIdRef = useRef<string | null>(sessionId)
  sessionIdRef.current = sessionId
  // 会话列表镜像（事件回调内读取最新值，避免订阅随列表变化重建）
  const sessionsRef = useRef<SessionSummary[]>(state.sessions)
  sessionsRef.current = state.sessions

  // 用快照初始化/刷新当前 session 的状态（快照中的 session 字段携带真实 id）
  const applySnapshot = useCallback((snapshot: SnapshotView) => {
    setState((prev) => ({
      ...prev,
      items: messagesToItems(snapshot.messages),
      model: snapshot.model,
      reasoning: snapshot.reasoning,
      contextTokens: snapshot.context_tokens,
      running: snapshot.running,
      queue: snapshot.queue,
      session: snapshot.session,
      question: snapshot.pending_question ?? null,
      error: null,
      goal: snapshot.goal ?? null,
      stats: {
        rounds: snapshot.rounds ?? 0,
        total_steps: snapshot.total_steps ?? 0,
        llm_time_ms: snapshot.llm_time_ms ?? 0,
        tool_time_ms: snapshot.tool_time_ms ?? 0,
        avg_first_token_ms: snapshot.avg_first_token_ms ?? 0,
        output_token_rate: snapshot.output_token_rate ?? 0,
        cache_hit_ratio: snapshot.cache_hit_ratio ?? 0,
        input_tokens: snapshot.input_tokens ?? 0,
        output_tokens: snapshot.output_tokens ?? 0,
        subagent_count: snapshot.subagent_count ?? 0,
      },
    }))
  }, [])

  // 服务端事件的统一入口（仅当前查看 session 的生命周期事件驱动 UI）
  const applyEvent = useCallback((event: ServerEvent) => {
    // 跳过不属于当前 session 的生命周期事件（其他 session 在后台运行）
    const currentSid = sessionIdRef.current
    if (
      'session_id' in event &&
      event.session_id &&
      currentSid &&
      event.session_id !== currentSid
    ) {
      return
    }

    setState((prev) => {
      const items = applyServerEvent(prev.items, event)
      switch (event.type) {
        case 'run_started':
          return { ...prev, items, running: true }
        case 'run_finished':
          return { ...prev, items, running: false }
        case 'queue_changed':
          // steering 队列全量快照：整体替换（队列短小，免增量合并）
          return { ...prev, items, queue: event.queue }
        case 'goal_changed':
          // goal 状态：启动携带目标原文；完成/取消清除徽标（完成汇报经
          // goal_done 工具卡片在消息流中可见）
          return {
            ...prev,
            items,
            goal: event.status === 'started' ? (event.objective ?? null) : null,
          }
        case 'question':
          return { ...prev, items, question: { id: event.id, question: event.question } }
        case 'question_cancelled':
          return {
            ...prev,
            items,
            question: prev.question?.id === event.id ? null : prev.question,
          }
        case 'error':
          return { ...prev, items, running: false, error: event.message }
        case 'agent': {
          // MessageEnd/AgentEnd/CompactionEnd 携带权威 context_tokens，
          // 运行中实时驱动 ContextRing（否则只有快照刷新时才更新）
          const contextTokens = agentEventContextTokens(event.event)
          return contextTokens === null
            ? { ...prev, items }
            : { ...prev, items, contextTokens }
        }
        default:
          return prev
      }
    })
  }, [])

  // 拉取指定 session 的快照并刷新 UI（boot / 切换 / refresh 共用）
  const loadSession = useCallback(
    async (id: string) => {
      const { session_id, snapshot } = await api.state(id)
      setState((prev) => ({ ...prev, sessionId: session_id }))
      applySnapshot(snapshot)
    },
    [applySnapshot],
  )

  const refreshSessions = useCallback(async () => {
    try {
      const sessions = await api.sessions()
      setState((prev) => ({ ...prev, sessions }))
    } catch {
      // 会话列表加载失败不阻塞主流程（侧栏显示空态）
    }
  }, [])

  const refreshWorkspaces = useCallback(async () => {
    try {
      const workspaces = await api.workspaces()
      setState((prev) => ({ ...prev, workspaces }))
    } catch {
      // workspace 列表加载失败不阻塞主流程（分组退化为纯会话视图）
    }
  }, [])

  // 回到启动页（当前查看的 session 被删除时）；保留已拉取的列表
  const resetView = useCallback(() => {
    setState((prev) => ({
      ...initialState,
      sessions: prev.sessions,
      workspaces: prev.workspaces,
    }))
  }, [])

  // 事件订阅（mount 时注册一次，整个生命周期有效）
  useEffect(() => {
    return api.subscribe((event) => {
      if (event.type === 'refresh') {
        // 落后/重连：重新拉取当前 session 快照
        const sid = sessionIdRef.current
        if (sid) {
          void api.state(sid).then(({ snapshot }) => applySnapshot(snapshot))
        }
        void refreshSessions()
        void refreshWorkspaces()
      } else if (event.type === 'session_created') {
        // 新 session 创建：刷新会话列表（可能伴随新 workspace，一并刷新）
        void refreshSessions()
        void refreshWorkspaces()
      } else if (event.type === 'session_deleted') {
        // 会话被删除（含 workspace 级联）：刷新列表；正在查看则回启动页
        void refreshSessions()
        void refreshWorkspaces()
        if (sessionIdRef.current === event.id) resetView()
      } else if (event.type === 'session_renamed') {
        // 重命名：刷新列表；当前查看的 session 同步顶栏标题
        void refreshSessions()
        if (sessionIdRef.current === event.id) {
          setState((prev) =>
            prev.session
              ? { ...prev, session: { id: prev.session.id, title: event.title } }
              : prev,
          )
        }
      } else if (event.type === 'workspace_deleted') {
        // 工作区被删除：刷新列表；当前查看的 session 属于该 workspace 时回启动页
        // （其 session 已打开时另有 session_deleted 广播兜底，此处覆盖未打开
        // 但仍在列表中的情况）
        void refreshSessions()
        void refreshWorkspaces()
        const sid = sessionIdRef.current
        if (
          sid &&
          sessionsRef.current.some((s) => s.id === sid && s.workspace_id === event.id)
        ) {
          resetView()
        }
      } else if (event.type === 'switch_model_ack') {
        // 模型切换确认：以服务端快照回填 model/reasoning（ack 只携带精简
        // ModelChoice，不含推理级别；且切换在 ack 前已落到共享视图，
        // 此刻快照即为权威）
        const sid = sessionIdRef.current
        if (sid && event.session_id === sid) {
          void api.state(sid).then(({ snapshot }) => applySnapshot(snapshot))
        }
      } else {
        applyEvent(event)
        // run 结束刷新会话列表（活跃度变化）
        if (event.type === 'run_finished') {
          void refreshSessions()
        }
      }
    })
  }, [applyEvent, applySnapshot, refreshSessions, refreshWorkspaces, resetView])

  // 挂载：确保 WebSocket 连接 → 拉取会话与 workspace 列表。
  // 不加载默认 session（无默认 workspace）：启动页由用户选择 workspace 后
  // 显式创建 session，或从侧栏恢复历史 session。
  useEffect(() => {
    let cancelled = false
    const boot = async () => {
      await api.connect()
      if (cancelled) return
      void refreshSessions()
      void refreshWorkspaces()
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
  }, [refreshSessions, refreshWorkspaces])

  const send = useCallback(async (text: string, images?: ImageContent[]) => {
    const trimmed = text.trim()
    const sid = sessionIdRef.current
    if (!trimmed || !sid) return
    try {
      // 运行中提交由服务端入 steering 队列并经 queue_changed 广播，
      // 前端无需本地预调队列状态
      await api.prompt(sid, trimmed, images)
    } catch (error) {
      setState((prev) => ({
        ...prev,
        error: error instanceof Error ? error.message : String(error),
      }))
    }
  }, [])

  const stop = useCallback(async () => {
    const sid = sessionIdRef.current
    if (sid) api.cancel(sid)
  }, [])

  const newSession = useCallback(
    async (workspace: string) => {
      try {
        const { id } = await api.createSession(workspace)
        // 拉取新 session 快照并切换查看（其事件流已自动并入当前连接）
        await loadSession(id)
        await refreshSessions()
        await refreshWorkspaces()
      } catch (error) {
        setState((prev) => ({
          ...prev,
          error: error instanceof Error ? error.message : String(error),
        }))
      }
    },
    [loadSession, refreshSessions, refreshWorkspaces],
  )

  /** 启动页首条消息：在选定 workspace 下创建 session，切换到它并发送。 */
  const startSession = useCallback(
    async (workspace: string, text: string, images?: ImageContent[]) => {
      const trimmed = text.trim()
      if (!workspace || !trimmed) return
      try {
        const { id } = await api.createSession(workspace)
        // 先切换查看（快照为空会话），再提交 prompt：后续流式事件
        // 经 applyEvent 增量驱动 UI
        await loadSession(id)
        await api.prompt(id, trimmed, images)
        await refreshSessions()
        await refreshWorkspaces()
      } catch (error) {
        setState((prev) => ({
          ...prev,
          error: error instanceof Error ? error.message : String(error),
        }))
      }
    },
    [loadSession, refreshSessions, refreshWorkspaces],
  )

  /** 登记新 workspace；失败时抛出（调用方就地展示错误）。 */
  const addWorkspace = useCallback(
    async (path: string) => {
      await api.createWorkspace(path)
      await refreshWorkspaces()
    },
    [refreshWorkspaces],
  )

  /** 删除 session（物理删除）；列表刷新与视图跳转经广播事件回填，
      失败时抛出（调用方就地展示错误）。 */
  const deleteSession = useCallback(async (id: string) => {
    await api.deleteSession(id)
  }, [])

  /** 重命名 session（空白标题 = 清除自定义，回退派生标题）；列表与顶栏
      标题经广播事件回填，失败时抛出（调用方就地展示错误）。 */
  const renameSession = useCallback(async (id: string, title: string) => {
    await api.renameSession(id, title)
  }, [])

  /** 删除 workspace（非空须 force 级联）；列表刷新与视图跳转经广播事件
      回填，失败时抛出（调用方就地展示错误）。 */
  const deleteWorkspace = useCallback(async (id: string, force: boolean) => {
    await api.deleteWorkspace(id, force)
  }, [])

  const resumeSession = useCallback(
    async (id: string) => {
      try {
        await loadSession(id)
      } catch (error) {
        setState((prev) => ({
          ...prev,
          error: error instanceof Error ? error.message : String(error),
        }))
      }
    },
    [loadSession],
  )

  const switchModel = useCallback(async (spec: string, reasoning?: string) => {
    const sid = sessionIdRef.current
    if (sid) api.switchModel(sid, spec, reasoning)
  }, [])

  const answerQuestion = useCallback(async (id: string, answer: AskUserAnswer) => {
    const sid = sessionIdRef.current
    if (sid) api.answerQuestion(sid, id, answer)
    setState((prev) => ({ ...prev, question: null }))
  }, [])

  /** 编辑 steering 队列条目原文（空文本 = 删除；变更经 queue_changed 回填）。 */
  const updateQueueEntry = useCallback((id: string, text: string) => {
    const sid = sessionIdRef.current
    if (sid) api.updateQueueEntry(sid, id, text)
  }, [])

  /** 删除 steering 队列条目。 */
  const removeQueueEntry = useCallback((id: string) => {
    const sid = sessionIdRef.current
    if (sid) api.removeQueueEntry(sid, id)
  }, [])

  /** 移动 steering 队列条目（上移/下移一位）。 */
  const moveQueueEntry = useCallback((id: string, direction: 'up' | 'down') => {
    const sid = sessionIdRef.current
    if (sid) api.moveQueueEntry(sid, id, direction)
  }, [])

  const dismissError = useCallback(() => {
    setState((prev) => ({ ...prev, error: null }))
  }, [])

  return {
    ...state,
    send,
    stop,
    newSession,
    startSession,
    addWorkspace,
    deleteSession,
    renameSession,
    deleteWorkspace,
    resumeSession,
    switchModel,
    answerQuestion,
    updateQueueEntry,
    removeQueueEntry,
    moveQueueEntry,
    dismissError,
  }
}

export type UseChat = ReturnType<typeof useChat>
