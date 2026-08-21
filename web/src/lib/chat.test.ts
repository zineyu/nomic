// chat.ts 纯函数测试：事件规整（流式累积/定稿/工具/压缩）与历史快照渲染。

import { describe, expect, it } from 'vitest'

import {
  agentEventContextTokens,
  applyAgentEvent,
  applyServerEvent,
  assistantText,
  messagesToItems,
  userContent,
  type ChatItem,
} from './chat'
import type { Message, ServerEvent, StopReason } from './types'

function userMessage(text: string): Message {
  return { role: 'user', content: text, timestamp: 1 }
}

function assistantMessage(text: string, stop: StopReason = 'stop'): Message {
  return {
    role: 'assistant',
    content: [{ type: 'text', text }],
    api: 'open_ai_completions',
    provider: 'openai',
    model: 'gpt-4o',
    usage: {
      input: 10,
      output: 5,
      cache_read: 0,
      cache_write: 0,
      total_tokens: 15,
      cost: { input: 0, output: 0, cache_read: 0, cache_write: 0, total: 0 },
    },
    stop_reason: stop,
    timestamp: 2,
  }
}

function toolResult(toolCallId: string, name: string, text: string, isError = false): Message {
  return {
    role: 'tool_result',
    tool_call_id: toolCallId,
    tool_name: name,
    content: [{ type: 'text', text }],
    is_error: isError,
    timestamp: 3,
  }
}

describe('userContent', () => {
  it('解析纯文本', () => {
    const { text, images } = userContent({ role: 'user', content: 'hi' })
    expect(text).toBe('hi')
    expect(images).toHaveLength(0)
  })

  it('解析文本 + 图片块', () => {
    const { text, images } = userContent({
      role: 'user',
      content: [
        { type: 'image', data: 'AAA', mime_type: 'image/png' },
        { type: 'text', text: '看图' },
      ],
    })
    expect(text).toBe('看图')
    expect(images).toEqual([{ data: 'AAA', mime_type: 'image/png' }])
  })
})

describe('messagesToItems', () => {
  it('快照消息转渲染项（user/assistant/tool_result）', () => {
    const items = messagesToItems([
      userMessage('你好'),
      assistantMessage('你好！'),
      toolResult('t1', 'bash', 'ok'),
    ])
    expect(items.map((i) => i.type)).toEqual(['user', 'assistant', 'tool'])
    const assistant = items[1]
    if (assistant.type === 'assistant') {
      expect(assistant.text).toBe('你好！')
      expect(assistant.streaming).toBe(false)
    }
    const tool = items[2]
    if (tool.type === 'tool') {
      expect(tool.name).toBe('bash')
      expect(tool.status).toBe('done')
      expect(tool.resultPreview).toBe('ok')
    }
  })

  it('从历史 assistant 的 tool_call 块恢复工具参数', () => {
    const assistantWithTool = assistantMessage('调用工具') as Extract<Message, { role: 'assistant' }>
    assistantWithTool.content = [
      { type: 'text', text: '调用工具' },
      { type: 'tool_call', id: 't1', name: 'bash', arguments: { command: 'ls -la' } },
    ]
    const items = messagesToItems([assistantWithTool, toolResult('t1', 'bash', 'ok')])
    expect(items.map((i) => i.type)).toEqual(['assistant', 'tool'])
    const tool = items[1]
    if (tool.type === 'tool') {
      expect(tool.args).toEqual({ command: 'ls -la' })
      expect(tool.status).toBe('done')
      expect(tool.resultPreview).toBe('ok')
    }
  })
})

describe('applyAgentEvent / applyServerEvent', () => {
  it('user 消息事件追加用户项', () => {
    const items = applyAgentEvent([], { MessageStart: userMessage('问题') })
    expect(items).toHaveLength(1)
    expect(items[0]).toMatchObject({ type: 'user', text: '问题' })
  })

  it('流式文本增量累积到流式 assistant 项', () => {
    let items: ChatItem[] = []
    items = applyAgentEvent(items, { MessageStart: assistantMessage('') })
    items = applyAgentEvent(items, {
      MessageUpdate: { TextDelta: { index: 0, delta: 'hi' } },
    })
    items = applyAgentEvent(items, {
      MessageUpdate: { TextDelta: { index: 0, delta: '!' } },
    })
    const item = items[0]
    if (item.type === 'assistant') {
      expect(item.text).toBe('hi!')
      expect(item.streaming).toBe(true)
    }
  })

  it('思考增量累积到 thinking 字段', () => {
    let items: ChatItem[] = []
    items = applyAgentEvent(items, { MessageStart: assistantMessage('') })
    items = applyAgentEvent(items, {
      MessageUpdate: { ThinkingDelta: { index: 0, delta: '想想' } },
    })
    const item = items[0]
    if (item.type === 'assistant') expect(item.thinking).toBe('想想')
  })

  it('MessageEnd 定稿替换流式项（文本/状态/模型）', () => {
    let items: ChatItem[] = []
    items = applyAgentEvent(items, { MessageStart: assistantMessage('') })
    items = applyAgentEvent(items, {
      MessageUpdate: { TextDelta: { index: 0, delta: 'partial' } },
    })
    items = applyAgentEvent(items, { MessageEnd: { message: assistantMessage('final'), context_tokens: 100 } })
    const item = items[0]
    if (item.type === 'assistant') {
      expect(item.text).toBe('final')
      expect(item.streaming).toBe(false)
      expect(item.model).toBe('gpt-4o')
      expect(item.usage?.total_tokens).toBe(15)
    }
  })

  it('失败响应（aborted）保留错误信息', () => {
    const failed = assistantMessage('', 'aborted') as Extract<Message, { role: 'assistant' }>
    failed.error_message = '用户取消'
    let items: ChatItem[] = []
    items = applyAgentEvent(items, { MessageStart: assistantMessage('') })
    items = applyAgentEvent(items, { MessageEnd: { message: failed, context_tokens: 0 } })
    const item = items[0]
    if (item.type === 'assistant') {
      expect(item.stopReason).toBe('aborted')
      expect(item.errorMessage).toBe('用户取消')
    }
  })

  it('工具执行事件驱动工具项（开始→更新→结束）', () => {
    let items: ChatItem[] = []
    items = applyAgentEvent(items, {
      ToolExecutionStart: {
        tool_call_id: 't1',
        tool_name: 'bash',
        args: { command: 'ls' },
      },
    })
    expect(items[0]).toMatchObject({ type: 'tool', name: 'bash', status: 'running' })
    items = applyAgentEvent(items, {
      ToolExecutionUpdate: {
        tool_call_id: 't1',
        tool_name: 'bash',
        partial: { content: [{ type: 'text', text: 'part' }] },
      },
    })
    items = applyAgentEvent(items, {
      ToolExecutionEnd: {
        tool_call_id: 't1',
        tool_name: 'bash',
        result: { content: [{ type: 'text', text: 'done' }], terminate: false },
        is_error: false,
      },
    })
    const tool = items[0]
    if (tool.type === 'tool') {
      expect(tool.status).toBe('done')
      expect(tool.resultPreview).toBe('done')
    }
  })

  it('ToolExecutionStart 合并已有工具项并保留结果预览', () => {
    const assistantWithTool = assistantMessage('调用') as Extract<Message, { role: 'assistant' }>
    assistantWithTool.content = [
      { type: 'tool_call', id: 't1', name: 'read', arguments: { path: 'a.rs' } },
    ]
    let items = messagesToItems([assistantWithTool])
    items = applyAgentEvent(items, {
      ToolExecutionStart: { tool_call_id: 't1', tool_name: 'read', args: { path: 'b.rs' } },
    })
    const tool = items[0]
    if (tool.type === 'tool') {
      expect(tool.args).toEqual({ path: 'b.rs' })
      expect(tool.status).toBe('running')
    }
  })

  it('MessageEnd 的 tool_result 保留已有工具参数', () => {
    const assistantWithTool = assistantMessage('调用') as Extract<Message, { role: 'assistant' }>
    assistantWithTool.content = [
      { type: 'tool_call', id: 't1', name: 'bash', arguments: { command: 'ls' } },
    ]
    let items = messagesToItems([assistantWithTool])
    items = applyAgentEvent(items, {
      MessageEnd: { message: toolResult('t1', 'bash', 'ok'), context_tokens: 0 },
    })
    const tool = items[0]
    if (tool.type === 'tool') {
      expect(tool.args).toEqual({ command: 'ls' })
      expect(tool.status).toBe('done')
      expect(tool.resultPreview).toBe('ok')
    }
  })

  it('工具执行错误标记 is_error', () => {
    let items: ChatItem[] = []
    items = applyAgentEvent(items, {
      ToolExecutionStart: { tool_call_id: 't2', tool_name: 'read', args: {} },
    })
    items = applyAgentEvent(items, {
      ToolExecutionEnd: {
        tool_call_id: 't2',
        tool_name: 'read',
        result: { content: [{ type: 'text', text: 'not found' }], terminate: false },
        is_error: true,
      },
    })
    const tool = items[0]
    if (tool.type === 'tool') {
      expect(tool.status).toBe('error')
      expect(tool.isError).toBe(true)
    }
  })

  it('压缩事件追加系统提示项', () => {
    const items = applyAgentEvent([], { CompactionStart: { tokens_before: 1000 } })
    expect(items[0]).toMatchObject({ type: 'system' })
    const after = applyAgentEvent(items, {
      CompactionEnd: {
        summary: 's',
        tokens_before: 1000,
        context_tokens: 200,
        kept_count: 5,
        usage: {
          input: 0,
          output: 0,
          cache_read: 0,
          cache_write: 0,
          total_tokens: 0,
          cost: { input: 0, output: 0, cache_read: 0, cache_write: 0, total: 0 },
        },
      },
    })
    expect(after[1]).toMatchObject({ type: 'system' })
  })

  it('applyServerEvent 透传 agent 事件并忽略运行/提问事件', () => {
    const events: ServerEvent[] = [
      { type: 'run_started', session_id: 's1' },
      { type: 'agent', session_id: 's1', event: { MessageStart: userMessage('x') } },
      { type: 'question', session_id: 's1', id: 'q1', question: { question: '继续？', kind: 'single_choice', options: ['是'] } },
    ]
    const items = events.reduce<ChatItem[]>((acc, e) => applyServerEvent(acc, e), [])
    expect(items).toHaveLength(1)
    expect(items[0]).toMatchObject({ type: 'user', text: 'x' })
  })
})

describe('assistantText', () => {
  it('提取文本与思考', () => {
    const { text, thinking } = assistantText({
      role: 'assistant',
      content: [
        { type: 'thinking', thinking: '推理' },
        { type: 'text', text: '结论' },
        { type: 'tool_call', id: 't', name: 'read', arguments: {} },
      ],
    } as never)
    expect(text).toBe('结论')
    expect(thinking).toBe('推理')
  })
})

describe('agentEventContextTokens', () => {
  it('从 MessageEnd / AgentEnd / CompactionEnd 提取 context_tokens', () => {
    expect(
      agentEventContextTokens({
        MessageEnd: { message: assistantMessage('ok'), context_tokens: 12_345 },
      }),
    ).toBe(12_345)
    expect(
      agentEventContextTokens({ AgentEnd: { messages: [], context_tokens: 12_400 } }),
    ).toBe(12_400)
    expect(
      agentEventContextTokens({
        CompactionEnd: {
          summary: 's',
          tokens_before: 56_000,
          context_tokens: 8_000,
          kept_count: 3,
          usage: {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 0,
            cost: { input: 0, output: 0, cache_read: 0, cache_write: 0, total: 0 },
          },
        },
      }),
    ).toBe(8_000)
  })

  it('其他事件返回 null', () => {
    expect(agentEventContextTokens('AgentStart')).toBeNull()
    expect(agentEventContextTokens('TurnStart')).toBeNull()
    expect(agentEventContextTokens({ MessageStart: userMessage('x') })).toBeNull()
  })
})
