// todo.ts 纯函数测试：todo_write 参数解析、当前清单推导（全量替换语义、
// 失败调用跳过、嵌套子 todo）、完成数统计。

import { describe, expect, it } from 'vitest'

import { countTodos, currentTodos, parseTodos } from './todo'
import type { ChatItem } from './chat'

function todoWrite(
  toolCallId: string,
  todos: unknown,
  status: 'running' | 'done' | 'error' = 'done',
): ChatItem {
  return {
    type: 'tool',
    id: toolCallId,
    toolCallId,
    name: 'todo_write',
    args: { todos },
    status,
    resultPreview: '',
    isError: status === 'error',
  }
}

describe('parseTodos', () => {
  it('解析扁平清单与嵌套子 todo', () => {
    const todos = parseTodos([
      { title: '父任务', status: 'in_progress', children: [{ title: '子任务', status: 'pending' }] },
      { title: '独立任务', status: 'completed' },
    ])
    expect(todos).toHaveLength(2)
    expect(todos[0].title).toBe('父任务')
    expect(todos[0].status).toBe('in_progress')
    expect(todos[0].children).toHaveLength(1)
    expect(todos[0].children[0].title).toBe('子任务')
    expect(todos[1].status).toBe('completed')
    // 树路径 key 稳定且唯一
    expect(todos[0].key).toBe('0')
    expect(todos[0].children[0].key).toBe('0.0')
  })

  it('非法条目丢弃，未知状态退化为 pending', () => {
    const todos = parseTodos([
      'not-an-object',
      { title: '' },
      { title: '缺状态' },
      { title: '未知状态', status: 'weird' },
    ])
    expect(todos).toHaveLength(2)
    expect(todos[0].status).toBe('pending')
    expect(todos[1].status).toBe('pending')
  })

  it('非数组输入返回空清单', () => {
    expect(parseTodos(null)).toEqual([])
    expect(parseTodos({})).toEqual([])
  })
})

describe('currentTodos', () => {
  it('无 todo_write 调用返回空清单', () => {
    const items: ChatItem[] = [
      { type: 'user', id: 'u1', text: 'hi', images: [], timestamp: 1 },
    ]
    expect(currentTodos(items)).toEqual([])
  })

  it('以最后一次 todo_write 为准（全量替换语义）', () => {
    const items: ChatItem[] = [
      todoWrite('c1', [{ title: '旧任务', status: 'pending' }]),
      todoWrite('c2', [{ title: '新任务', status: 'in_progress' }]),
    ]
    const todos = currentTodos(items)
    expect(todos).toHaveLength(1)
    expect(todos[0].title).toBe('新任务')
  })

  it('失败的 todo_write 不改变清单（回退到上一次成功调用）', () => {
    const items: ChatItem[] = [
      todoWrite('c1', [{ title: '有效清单', status: 'pending' }]),
      todoWrite('c2', [{ title: '失败写入', status: 'pending' }], 'error'),
    ]
    const todos = currentTodos(items)
    expect(todos).toHaveLength(1)
    expect(todos[0].title).toBe('有效清单')
  })

  it('运行中的 todo_write 参数即时反映（乐观展示）', () => {
    const items: ChatItem[] = [
      todoWrite('c1', [{ title: '写入中', status: 'pending' }], 'running'),
    ]
    expect(currentTodos(items)).toHaveLength(1)
  })

  it('空 todos 数组 = 清空清单', () => {
    const items: ChatItem[] = [
      todoWrite('c1', [{ title: '任务', status: 'pending' }]),
      todoWrite('c2', []),
    ]
    expect(currentTodos(items)).toEqual([])
  })

  it('忽略其他工具的调用', () => {
    const items: ChatItem[] = [
      todoWrite('c1', [{ title: '任务', status: 'pending' }]),
      {
        type: 'tool',
        id: 'c2',
        toolCallId: 'c2',
        name: 'bash',
        args: { command: 'ls', todos: [{ title: '伪装', status: 'pending' }] },
        status: 'done',
        resultPreview: '',
        isError: false,
      },
    ]
    const todos = currentTodos(items)
    expect(todos[0].title).toBe('任务')
  })
})

describe('countTodos', () => {
  it('统计总数与完成数（含嵌套；cancelled 计入完成）', () => {
    const todos = parseTodos([
      {
        title: '父',
        status: 'in_progress',
        children: [
          { title: '子1', status: 'completed' },
          { title: '子2', status: 'cancelled' },
        ],
      },
      { title: 'pending 任务', status: 'pending' },
    ])
    expect(countTodos(todos)).toEqual({ total: 4, done: 2 })
  })
})
