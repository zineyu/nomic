// REST 客户端 + SSE 客户端。
//
// 生产（nomic --web 伺服 dist）同源访问 /api；开发期经 Vite 代理转发到
// nomic 服务（见 vite.config.ts 的 proxy）。SSE 用 fetch + ReadableStream
// 手动解析：可控制断线退避重连与自定义 event 处理（EventSource 不支持
// 自定义 header，且重连语义不可控）。

import type {
  AskUserAnswer,
  ModelsResponse,
  ServerEvent,
  SessionSummary,
  StateResponse,
} from './types'

const API_BASE = '/api'

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${API_BASE}${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...init,
  })
  if (!response.ok) {
    let detail = response.statusText
    try {
      const body = (await response.json()) as { error?: string }
      detail = body.error ?? detail
    } catch {
      // 非 JSON 错误体，保留 statusText
    }
    throw new Error(`API ${init?.method ?? 'GET'} ${path}: ${response.status} ${detail}`)
  }
  return (await response.json()) as T
}

export const api = {
  state: () => request<StateResponse>('/state'),
  sessions: () => request<SessionSummary[]>('/sessions'),
  createSession: () => request<{ id: string; title: string | null }>('/sessions', { method: 'POST' }),
  resumeSession: (id: string) =>
    request<{ id: string; title: string | null }>('/sessions/resume', {
      method: 'POST',
      body: JSON.stringify({ id }),
    }),
  models: () => request<ModelsResponse>('/models'),
  switchModel: (spec: string, reasoning?: string) =>
    request<unknown>('/models', {
      method: 'POST',
      body: JSON.stringify({ spec, reasoning: reasoning ?? null }),
    }),
  prompt: (text: string) =>
    request<{ status: 'started' | 'queued' }>('/prompt', {
      method: 'POST',
      body: JSON.stringify({ text }),
    }),
  cancel: () => request<{ cancelled: boolean }>('/cancel', { method: 'POST' }),
  answerQuestion: (id: string, answer: AskUserAnswer) =>
    request<{ ok: boolean }>(`/question/${id}`, {
      method: 'POST',
      body: JSON.stringify(answer),
    }),
}

export type StreamEvent = ServerEvent | { type: 'refresh' }

export interface StreamClient {
  /** 连接事件流；返回断开函数。断线自动退避重连。 */
  connect(onEvent: (event: StreamEvent) => void): () => void
}

/** 解析一个 SSE 块（空行分隔）为 `{ event, data }`；无 data 时返回 `null`。 */
function parseSseBlock(block: string): { event: string; data: string } | null {
  let event = 'message'
  let data = ''
  for (const line of block.split('\n')) {
    if (line.startsWith('event:')) event = line.slice(6).trim()
    else if (line.startsWith('data:')) data += line.slice(5).trimStart()
  }
  if (!data) return null
  return { event, data }
}

export function createStreamClient(): StreamClient {
  return {
    connect(onEvent) {
      let closed = false
      let retry = 0
      let controller: AbortController | null = null

      const connectOnce = async () => {
        if (closed) return
        controller = new AbortController()
        try {
          const response = await fetch(`${API_BASE}/stream`, {
            signal: controller.signal,
            headers: { Accept: 'text/event-stream' },
          })
          if (!response.ok || !response.body) {
            throw new Error(`stream: ${response.status} ${response.statusText}`)
          }
          retry = 0
          const reader = response.body.getReader()
          const decoder = new TextDecoder()
          let buffer = ''
          while (true) {
            const { done, value } = await reader.read()
            if (done) break
            buffer += decoder.decode(value, { stream: true })
            const blocks = buffer.split('\n\n')
            buffer = blocks.pop() ?? ''
            for (const block of blocks) {
              const parsed = parseSseBlock(block)
              if (!parsed) continue
              if (parsed.event === 'refresh') {
                onEvent({ type: 'refresh' })
                continue
              }
              try {
                onEvent(JSON.parse(parsed.data) as ServerEvent)
              } catch {
                // 忽略无法解析的行
              }
            }
          }
        } catch (error) {
          if (closed || (error instanceof DOMException && error.name === 'AbortError')) return
          console.warn('SSE 断开，准备重连', error)
        } finally {
          controller = null
        }
        if (!closed) {
          retry += 1
          const delay = Math.min(1000 * 2 ** retry, 15_000)
          setTimeout(connectOnce, delay)
        }
      }

      void connectOnce()
      return () => {
        closed = true
        controller?.abort()
      }
    },
  }
}
