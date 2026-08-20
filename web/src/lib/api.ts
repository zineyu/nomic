// 纯 WebSocket 事件驱动通信客户端（单例）。
//
// 所有前端↔后端通信通过 `ws://{host}/ws` 双向事件流。服务端维护进程级全局事件
// 总线，连接后自动接收所有 session 的事件（每个事件携带 `session_id` 供路由）：
// - **查询类**（`get_state` / `list_models` / `list_sessions` / `list_workspaces`，
//   以及查询式命令 `create_workspace`）：携带 `request_id`，
//   服务端响应事件带同一 `request_id` 供关联。
// - **命令类**（`prompt` / `cancel` / `answer_question` / `switch_model` /
//   `create_session`）：携带 `session_id` 指定目标 session，fire-and-forget，
//   由服务端后续生命周期事件驱动前端状态更新。
//
// 连接生命周期：断线自动退避重连（指数退避上限 15s）；重连成功后向所有监听器
// 发送本地 `refresh` 事件（由 useChat 重新拉取快照）；请求超时 30s 自动拒绝。

import type {
  AskUserAnswer,
  ClientEvent,
  ImageContent,
  ModelChoice,
  ServerEvent,
  SessionSummary,
  SnapshotView,
  WorkspaceSummary,
} from './types'

type EventHandler = (event: ServerEvent) => void

type QueryEventInput =
  | { type: 'get_state'; session_id: string }
  | { type: 'list_models' }
  | { type: 'list_sessions' }
  | { type: 'list_workspaces' }
  | { type: 'create_workspace'; path: string }

class WsClient {
  private ws: WebSocket | null = null
  private pending = new Map<
    string,
    {
      resolve: (v: unknown) => void
      reject: (e: Error) => void
      timer: ReturnType<typeof setTimeout>
    }
  >()
  private eventHandlers = new Set<EventHandler>()
  private retry = 0
  private retryTimer: ReturnType<typeof setTimeout> | null = null
  private requestId = 0
  private connectResolvers: Array<() => void> = []
  private hasConnected = false

  /** 注册事件监听（生命周期与调用方一致，需手动退订）。 */
  subscribe(handler: EventHandler): () => void {
    this.eventHandlers.add(handler)
    return () => {
      this.eventHandlers.delete(handler)
    }
  }

  /** 确保 WebSocket 已连接（幂等）。连接就绪后 resolve。 */
  connect(): Promise<void> {
    if (this.ws?.readyState === WebSocket.OPEN) {
      return Promise.resolve()
    }
    return new Promise<void>((resolve) => {
      this.connectResolvers.push(resolve)
      this._connect()
    })
  }

  /** 发送客户端事件（连接未就绪时静默丢弃）。 */
  send(event: ClientEvent): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(event))
    }
  }

/** 发送查询事件并等待响应（通过 request_id 关联，30s 超时）。 */
  request<T>(event: QueryEventInput): Promise<T> {
    const id = `r${++this.requestId}`
    const fullEvent = { ...event, request_id: id } as ClientEvent

    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        reject(new Error(`请求超时: ${event.type}`))
      }, 30_000)
      this.pending.set(id, {
        resolve: resolve as (v: unknown) => void,
        reject,
        timer,
      })
      this.send(fullEvent)
    })
  }

  /** 清理：关闭连接、拒绝所有待处理请求。 */
  destroy(): void {
    this.disconnect()
    this.eventHandlers.clear()
  }

  // ── 连接管理 ──────────────────────────────────────────────────────

  private _connect(): void {
    if (this.ws?.readyState === WebSocket.OPEN) return

    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const ws = new WebSocket(`${proto}//${window.location.host}/ws`)
    this.ws = ws

    ws.onopen = () => {
      this.retry = 0
      for (const resolve of this.connectResolvers) resolve()
      this.connectResolvers = []
      // 重连（非首次连接）：事件流自动恢复，但断开期间的事件已丢失，
      // 通知监听器重新拉取快照
      if (this.hasConnected) {
        for (const handler of this.eventHandlers) {
          handler({ type: 'refresh' })
        }
      }
      this.hasConnected = true
    }

    ws.onmessage = (event: MessageEvent) => {
      if (typeof event.data !== 'string' || !event.data) return
      try {
        const msg = JSON.parse(event.data) as ServerEvent
        // 带 request_id 的响应事件 → 关联到 pending 请求
        if ('request_id' in msg && msg.request_id) {
          const entry = this.pending.get(msg.request_id)
          if (entry) {
            this.pending.delete(msg.request_id)
            clearTimeout(entry.timer)
            if (msg.type === 'error') {
              entry.reject(new Error(msg.message))
            } else {
              entry.resolve(msg)
            }
            return
          }
        }
        // 广播给所有事件监听器
        for (const handler of this.eventHandlers) {
          handler(msg)
        }
      } catch {
        // 忽略无法解析的消息
      }
    }

    ws.onerror = () => {
      // onclose 会在 onerror 之后触发，在那里处理重连
    }

    ws.onclose = () => {
      this.ws = null
      this.rejectAllPending('连接已断开')
      this.scheduleReconnect()
    }
  }

  private disconnect(): void {
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer)
      this.retryTimer = null
    }
    if (this.ws !== null) {
      this.ws.onclose = null // 阻止 onclose 触发重连
      this.ws.close()
      this.ws = null
    }
    this.rejectAllPending('连接已断开')
    this.connectResolvers = []
  }

  private scheduleReconnect(): void {
    this.retry += 1
    const delay = Math.min(1000 * 2 ** this.retry, 15_000)
    this.retryTimer = setTimeout(() => {
      this.retryTimer = null
      this._connect()
    }, delay)
  }

  private rejectAllPending(reason: string): void {
    for (const [, entry] of this.pending) {
      clearTimeout(entry.timer)
      entry.reject(new Error(reason))
    }
    this.pending.clear()
  }
}

// ── 单例导出 ─────────────────────────────────────────────────────────

const client = new WsClient()

export const api = {
  /** 注册事件监听（组件生命周期内有效，需退订）。 */
  subscribe: (handler: EventHandler) => client.subscribe(handler),

  /** 确保 WebSocket 已连接（幂等）。 */
  connect: () => client.connect(),

  /** 获取会话快照（查询类；返回解析后的真实 session_id 与快照）。 */
  state: (sessionId: string) =>
    client.request<{ session_id: string; snapshot: SnapshotView }>({
      type: 'get_state',
      session_id: sessionId,
    }),

  /** 列出全部 session 摘要。 */
  sessions: () =>
    client.request<{ sessions: SessionSummary[] }>({ type: 'list_sessions' }).then(
      (r) => r.sessions,
    ),

  /** 列出全部 workspace 摘要。 */
  workspaces: () =>
    client.request<{ workspaces: WorkspaceSummary[] }>({ type: 'list_workspaces' }).then(
      (r) => r.workspaces,
    ),

  /** 登记新 workspace（查询式命令；目录不存在时 reject 服务端错误消息）。 */
  createWorkspace: (path: string) =>
    client.request<{ id: string; path: string }>({ type: 'create_workspace', path }),

  /** 新建 session（命令类，等待 session_created 事件确认）。
      `workspace` 指定归属目录（不存在则登记新 workspace）；缺省归属服务端 cwd。 */
  createSession: (workspace?: string): Promise<{ id: string; title: string | null }> => {
    client.send({ type: 'create_session', workspace })
    return new Promise((resolve) => {
      const unsub = client.subscribe((event) => {
        if (event.type === 'session_created') {
          unsub()
          resolve({ id: event.id, title: event.title })
        }
      })
    })
  },

  /** 候选模型列表。 */
  models: () =>
    client.request<{ candidates: ModelChoice[] }>({ type: 'list_models' }).then(
      (r) => r.candidates,
    ),

  /** 切换会话模型（命令类，需指定 session_id）。 */
  switchModel: (sessionId: string, spec: string, reasoning?: string | null) => {
    client.send({
      type: 'switch_model',
      session_id: sessionId,
      spec,
      reasoning: reasoning ?? null,
    })
  },

  /** 提交 prompt（命令类，需指定 session_id，等待 prompt_ack 确认）。 */
  prompt: (
    sessionId: string,
    text: string,
    images?: ImageContent[],
  ): Promise<{ status: 'started' | 'queued' }> => {
    client.send({ type: 'prompt', session_id: sessionId, text, images: images ?? [] })
    return new Promise((resolve) => {
      const unsub = client.subscribe((event) => {
        if (event.type === 'prompt_ack' && event.session_id === sessionId) {
          unsub()
          resolve({ status: event.queued ? 'queued' : 'started' })
        }
      })
      // 超时兜底（防止 ack 丢失时 Promise 悬挂）
      setTimeout(() => {
        unsub()
        resolve({ status: 'started' })
      }, 5_000)
    })
  },

  /** 取消当前轮运行（命令类，需指定 session_id）。 */
  cancel: (sessionId: string) => {
    client.send({ type: 'cancel', session_id: sessionId })
  },

  /** 回答提问（命令类，需指定 session_id）。 */
  answerQuestion: (sessionId: string, id: string, answer: AskUserAnswer) => {
    client.send({
      type: 'answer_question',
      session_id: sessionId,
      id,
      answers: answer.answers,
      custom: answer.custom,
    })
  },
}
