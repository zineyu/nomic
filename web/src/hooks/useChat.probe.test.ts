// 临时探针测试：验证 useChat 对 agent 事件（context_tokens）与
// switch_model_ack 的响应。

import { act, renderHook } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

import type { ServerEvent } from '@/lib/types'

const handlers = new Set<(e: ServerEvent) => void>()

// 模拟服务端权威状态：切换后快照返回新模型（handle_switch_model 在发 ack
// 前已同步落共享视图，快照即为权威）
let serverModel = {
  id: 'm1',
  name: 'M1',
  api: 'open_ai_completions',
  provider: 'p1',
  base_url: '',
  reasoning: false,
  context_window: 1000,
  max_tokens: 100,
  cost_input: 0,
  cost_output: 0,
  cost_cache_read: 0,
  cost_cache_write: 0,
}

vi.mock('@/lib/api', () => ({
  api: {
    connect: vi.fn(() => Promise.resolve()),
    subscribe: (h: (e: ServerEvent) => void) => {
      handlers.add(h)
      return () => handlers.delete(h)
    },
    sessions: vi.fn(() => Promise.resolve([])),
    projects: vi.fn(() => Promise.resolve([])),
    state: vi.fn(() =>
      Promise.resolve({
        session_id: 's1',
        snapshot: {
          messages: [],
          model: serverModel,
          reasoning: null,
          context_tokens: 10,
          running: false,
          queue: [],
          session: { id: 's1', title: 't' },
          pending_question: null,
          project: '/tmp',
          goal: null,
        },
      }),
    ),
    prompt: vi.fn(),
    cancel: vi.fn(),
    createSession: vi.fn(),
    switchModel: vi.fn(),
    answerQuestion: vi.fn(),
    updateQueueEntry: vi.fn(),
    removeQueueEntry: vi.fn(),
    moveQueueEntry: vi.fn(),
  },
}))

import { useChat } from './useChat'

function emit(event: ServerEvent) {
  for (const h of handlers) h(event)
}

describe('useChat 探针', () => {
  beforeEach(() => {
    handlers.clear()
    serverModel = {
      ...serverModel,
      id: 'm1',
      name: 'M1',
      provider: 'p1',
      reasoning: false,
      context_window: 1000,
    }
  })

  it('agent MessageEnd 事件更新 contextTokens', async () => {
    const { result } = renderHook(() => useChat())
    await act(async () => {
      await result.current.resumeSession('s1')
    })
    expect(result.current.contextTokens).toBe(10)

    act(() => {
      emit({
        type: 'agent',
        session_id: 's1',
        event: {
          MessageEnd: {
            message: { role: 'user', content: 'hi', timestamp: 1 },
            context_tokens: 42,
          },
        },
      })
    })
    expect(result.current.contextTokens).toBe(42)
  })

  it('switch_model_ack 后 model 更新', async () => {
    const { result } = renderHook(() => useChat())
    await act(async () => {
      await result.current.resumeSession('s1')
    })
    expect(result.current.model?.id).toBe('m1')

    // 服务端已完成切换（ack 前落共享视图），快照返回新模型
    serverModel = {
      ...serverModel,
      id: 'm2',
      name: 'M2',
      provider: 'p2',
      reasoning: true,
      context_window: 2000,
    }
    act(() => {
      emit({
        type: 'switch_model_ack',
        session_id: 's1',
        choice: { provider: 'p2', id: 'm2', name: 'M2', context_window: 2000, reasoning: true },
      })
    })
    await act(async () => {})
    expect(result.current.model?.id).toBe('m2')
  })
})
