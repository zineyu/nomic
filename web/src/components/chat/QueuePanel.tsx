// steering 队列面板：输入框上方展示排队消息（用户输入原文），支持就地编辑
// 文本、删除、上移/下移——TUI QUEUE 模式的 web 版（ADR-0014 语义：运行中
// 入队的消息在当前步骤完成后逐条注入本轮运行；mention 投递时展开）。
//
// 队列状态由服务端权威驱动（queue_changed 全量快照），本地仅持有「正在
// 编辑」子状态；条目 id 单调递增不复用，被注入/删除的条目自然不再匹配
// 编辑子状态（无需清理）。

import { useState } from 'react'
import { Check, ChevronDown, ChevronUp, Pencil, X } from 'lucide-react'

import { Textarea } from '@/components/ui/textarea'
import type { QueueEntry } from '@/lib/types'

interface QueuePanelProps {
  queue: QueueEntry[]
  /** 保存条目原文（空文本 = 删除该条目，与服务端口径一致） */
  onUpdate: (id: string, text: string) => void
  onRemove: (id: string) => void
  onMove: (id: string, direction: 'up' | 'down') => void
}

/** 行内图标按钮（幽灵态：辅助操作降级为次要，hover 才显现） */
function RowButton({
  title,
  disabled,
  onClick,
  children,
}: {
  title: string
  disabled?: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      title={title}
      aria-label={title}
      disabled={disabled}
      onClick={onClick}
      className="rounded p-1 text-muted-foreground transition-colors outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-30"
    >
      {children}
    </button>
  )
}

export function QueuePanel({ queue, onUpdate, onRemove, onMove }: QueuePanelProps) {
  const [editingId, setEditingId] = useState<string | null>(null)
  const [draft, setDraft] = useState('')

  if (queue.length === 0) return null

  const beginEdit = (entry: QueueEntry) => {
    setEditingId(entry.id)
    setDraft(entry.text)
  }
  const saveEdit = () => {
    if (editingId) onUpdate(editingId, draft)
    setEditingId(null)
    setDraft('')
  }
  const cancelEdit = () => {
    setEditingId(null)
    setDraft('')
  }

  return (
    <div data-slot="queue-panel" className="mx-auto w-full max-w-page px-4 pb-2 sm:px-7">
      <div className="rounded-xl border bg-card px-3.5 py-2">
        <div className="pb-1 text-xs text-muted-foreground">
          已排队 {queue.length} 条
        </div>
        <ul className="space-y-0.5">
          {queue.map((entry, index) => (
            <li key={entry.id} className="rounded-md">
              {editingId === entry.id ? (
                // 就地编辑：Enter 保存（空文本删除）、Shift+Enter 换行、Esc 取消
                <div className="py-0.5">
                  <Textarea
                    autoFocus
                    rows={2}
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                        e.preventDefault()
                        saveEdit()
                      } else if (e.key === 'Escape') {
                        e.preventDefault()
                        cancelEdit()
                      }
                    }}
                    className="min-h-7 resize-none text-sm"
                  />
                  <div className="flex justify-end gap-1 pt-1">
                    <RowButton title="保存" onClick={saveEdit}>
                      <Check className="size-3.5" />
                    </RowButton>
                    <RowButton title="取消" onClick={cancelEdit}>
                      <X className="size-3.5" />
                    </RowButton>
                  </div>
                </div>
              ) : (
                <div className="group flex items-start gap-1 px-1 py-0.5">
                  <span className="line-clamp-3 min-w-0 flex-1 text-sm break-words whitespace-pre-wrap">
                    {entry.text}
                    {entry.images > 0 && (
                      <span className="ml-1 text-xs text-muted-foreground">
                        [图片 ×{entry.images}]
                      </span>
                    )}
                  </span>
                  <div className="flex shrink-0 items-center opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100">
                    <RowButton
                      title="上移"
                      disabled={index === 0}
                      onClick={() => onMove(entry.id, 'up')}
                    >
                      <ChevronUp className="size-3.5" />
                    </RowButton>
                    <RowButton
                      title="下移"
                      disabled={index === queue.length - 1}
                      onClick={() => onMove(entry.id, 'down')}
                    >
                      <ChevronDown className="size-3.5" />
                    </RowButton>
                    <RowButton title="编辑" onClick={() => beginEdit(entry)}>
                      <Pencil className="size-3.5" />
                    </RowButton>
                    <RowButton title="删除" onClick={() => onRemove(entry.id)}>
                      <X className="size-3.5" />
                    </RowButton>
                  </div>
                </div>
              )}
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
