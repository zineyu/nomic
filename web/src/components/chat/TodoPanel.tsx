// todo 面板：输入框上方展示当前任务清单及完成状态。
//
// 清单由消息项推导（lib/todo.ts::currentTodos，全量替换语义），无需服务端
// 额外状态。完成/取消的条目以删除线 + 置灰标识；进行中的条目以实心图标
// 强调；空清单不渲染。样式与 QueuePanel 同一形态（卡片 + 行列表）。

import { CheckCircle2, Circle, CircleDashed, XCircle } from 'lucide-react'

import { countTodos, type TodoEntry, type TodoStatus } from '@/lib/todo'

interface TodoPanelProps {
  todos: TodoEntry[]
}

/** 状态图标（消色差：以前景色阶梯表达状态，icons 统一 size-3.5） */
function StatusIcon({ status }: { status: TodoStatus }) {
  switch (status) {
    case 'completed':
      return <CheckCircle2 className="size-3.5 shrink-0 text-muted-foreground" aria-label="已完成" />
    case 'cancelled':
      return <XCircle className="size-3.5 shrink-0 text-muted-foreground" aria-label="已取消" />
    case 'in_progress':
      return <CircleDashed className="size-3.5 shrink-0 text-foreground" aria-label="进行中" />
    case 'pending':
      return <Circle className="size-3.5 shrink-0 text-muted-foreground" aria-label="未开始" />
  }
}

function TodoRow({ todo }: { todo: TodoEntry }) {
  const settled = todo.status === 'completed' || todo.status === 'cancelled'
  return (
    <li>
      <div className="flex items-start gap-2 px-1 py-0.5">
        <span className="pt-0.5">
          <StatusIcon status={todo.status} />
        </span>
        <span
          className={`min-w-0 flex-1 text-sm break-words ${
            settled ? 'text-muted-foreground line-through' : ''
          }`}
        >
          {todo.title}
        </span>
      </div>
      {todo.children.length > 0 && (
        <ul className="pl-6">
          {todo.children.map((child) => (
            <TodoRow key={child.key} todo={child} />
          ))}
        </ul>
      )}
    </li>
  )
}

export function TodoPanel({ todos }: TodoPanelProps) {
  if (todos.length === 0) return null
  const { total, done } = countTodos(todos)
  return (
    <div data-slot="todo-panel" className="mx-auto w-full max-w-page px-4 pb-2 sm:px-7">
      <div className="rounded-xl border bg-card px-3.5 py-2">
        <div className="pb-1 text-xs text-muted-foreground">
          任务清单 · {done}/{total} 已完成
        </div>
        <ul className="max-h-48 overflow-y-auto">
          {todos.map((todo) => (
            <TodoRow key={todo.key} todo={todo} />
          ))}
        </ul>
      </div>
    </div>
  )
}
