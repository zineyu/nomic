// todo 面板数据：从消息项列表推导当前 todo 清单。
//
// todo_write 采用全量替换语义（crates/app/nomic-tools/src/todo.rs）：每次
// 调用提交完整清单，因此「最后一次成功调用的参数」即当前权威状态。
// 历史快照经 messagesToItems 恢复了工具调用参数，resume 后同样可推导。
//
// 纯函数，可直接单测。

import type { ChatItem } from './chat'

export type TodoStatus = 'pending' | 'in_progress' | 'completed' | 'cancelled'

export interface TodoEntry {
  /** 渲染 key（树路径；todo_write 的 id 可省略，不保证存在） */
  key: string
  title: string
  status: TodoStatus
  children: TodoEntry[]
}

const STATUSES: ReadonlySet<string> = new Set([
  'pending',
  'in_progress',
  'completed',
  'cancelled',
])

/** 解析单条 todo（非法条目丢弃；未知状态退化为 pending）。 */
function parseTodo(value: unknown, key: string): TodoEntry | null {
  if (typeof value !== 'object' || value === null) return null
  const raw = value as Record<string, unknown>
  if (typeof raw['title'] !== 'string' || raw['title'] === '') return null
  const status =
    typeof raw['status'] === 'string' && STATUSES.has(raw['status'])
      ? (raw['status'] as TodoStatus)
      : 'pending'
  const children = Array.isArray(raw['children'])
    ? parseTodos(raw['children'], key)
    : []
  return { key, title: raw['title'], status, children }
}

/** 解析 todo 数组（`parentKey` 用于生成稳定的树路径 key）。 */
export function parseTodos(value: unknown, parentKey = ''): TodoEntry[] {
  if (!Array.isArray(value)) return []
  const todos: TodoEntry[] = []
  for (const [index, item] of value.entries()) {
    const todo = parseTodo(item, parentKey ? `${parentKey}.${index}` : `${index}`)
    if (todo) todos.push(todo)
  }
  return todos
}

/** 从消息项推导当前 todo 清单：以最后一次 todo_write 的参数为准
 *（失败的调用不改变清单，跳过；无调用 = 空清单）。 */
export function currentTodos(items: ChatItem[]): TodoEntry[] {
  for (let i = items.length - 1; i >= 0; i -= 1) {
    const item = items[i]
    if (item.type !== 'tool' || item.name !== 'todo_write' || item.status === 'error') {
      continue
    }
    if (Array.isArray(item.args['todos'])) {
      return parseTodos(item.args['todos'])
    }
  }
  return []
}

/** 统计清单总数与完成数（completed 计入完成；cancelled 视为已了结）。 */
export function countTodos(todos: TodoEntry[]): { total: number; done: number } {
  let total = 0
  let done = 0
  const walk = (entries: TodoEntry[]) => {
    for (const entry of entries) {
      total += 1
      if (entry.status === 'completed' || entry.status === 'cancelled') done += 1
      walk(entry.children)
    }
  }
  walk(todos)
  return { total, done }
}
