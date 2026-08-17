// 工具执行卡片：名称 + 参数摘要 + 状态（执行中/成功/失败），点击展开
// 完整参数与结果预览。

import { memo, useState } from 'react'
import { CheckCircle2, ChevronDown, Loader2, XCircle } from 'lucide-react'

import { cn } from '@/lib/utils'
import { briefArgs } from '@/lib/toolArgs'
import type { ToolItem } from '@/lib/chat'

function ToolCardImpl({ item }: { item: ToolItem }) {
  const [expanded, setExpanded] = useState(false)

  const icon =
    item.status === 'running' ? (
      <Loader2 className="size-3.5 animate-spin text-muted-foreground" />
    ) : item.status === 'error' ? (
      <XCircle className="size-3.5 text-destructive" />
    ) : (
      <CheckCircle2 className="size-3.5 text-success" />
    )

  const hasDetail = Object.keys(item.args).length > 0 || item.resultPreview !== ''

  return (
    <div
      className={cn(
        'mx-auto max-w-3xl overflow-hidden rounded-xl border bg-card text-card-foreground shadow-sm',
        item.isError && 'border-destructive/50',
      )}
    >
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        disabled={!hasDetail}
        className={cn(
          'flex w-full items-center gap-2 px-3 py-2.5 text-left text-xs',
          hasDetail ? 'cursor-pointer hover:bg-accent/60' : 'cursor-default',
        )}
      >
        {icon}
        <span className="font-mono font-medium">{item.name}</span>
        <span className="min-w-0 flex-1 truncate text-muted-foreground">
          {briefArgs(item.name, item.args)}
        </span>
        {item.status === 'running' ? (
          <span className="shrink-0 text-muted-foreground">执行中…</span>
        ) : hasDetail ? (
          <ChevronDown
            className={cn(
              'size-3.5 shrink-0 text-muted-foreground transition-transform',
              expanded && 'rotate-180',
            )}
          />
        ) : null}
      </button>
      {expanded && (
        <div className="space-y-2 border-t px-3 py-2">
          {Object.keys(item.args).length > 0 && (
            <pre className="max-h-56 overflow-auto rounded-lg bg-muted/50 p-2 font-mono text-xs leading-relaxed">
              {JSON.stringify(item.args, null, 2)}
            </pre>
          )}
          {item.resultPreview !== '' && (
            <pre className="max-h-56 overflow-auto whitespace-pre-wrap rounded-lg bg-muted/50 p-2 font-mono text-xs leading-relaxed text-muted-foreground">
              {item.resultPreview}
            </pre>
          )}
        </div>
      )}
    </div>
  )
}

// memo：工具卡片状态在单次执行周期内不频繁变化，避免随流式文本重渲染。
export const ToolCard = memo(ToolCardImpl)
