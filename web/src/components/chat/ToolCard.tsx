// 工具执行卡片：终端图标 + 工具名 + 参数摘要 + 状态徽章（✓ 完成 /
// ● 运行中 / ✗ 失败），点击展开完整参数与结果预览。

import { memo, useState } from 'react'
import { ChevronDown, Loader2, Terminal } from 'lucide-react'

import { cn } from '@/lib/utils'
import { briefArgs } from '@/lib/toolArgs'
import type { ToolItem } from '@/lib/chat'

function StatusBadge({ item }: { item: ToolItem }) {
  if (item.status === 'running') {
    return (
      <span className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border bg-muted px-2.5 py-0.5 text-xs text-muted-foreground">
        <Loader2 className="size-2.5 animate-spin" />
        运行中
      </span>
    )
  }
  if (item.status === 'error') {
    return (
      <span className="shrink-0 rounded-full border border-destructive/40 bg-destructive/10 px-2.5 py-0.5 text-xs text-destructive">
        ✗ 失败
      </span>
    )
  }
  return (
    <span className="shrink-0 rounded-full border border-success/40 bg-success/10 px-2.5 py-0.5 text-xs text-success">
      ✓ 完成
    </span>
  )
}

function ToolCardImpl({ item }: { item: ToolItem }) {
  const [expanded, setExpanded] = useState(false)
  const hasDetail = Object.keys(item.args).length > 0 || item.resultPreview !== ''

  return (
    <div
      className={cn(
        'max-w-[640px] overflow-hidden rounded-xl border bg-card text-card-foreground shadow-sm',
        item.isError && 'border-destructive/50',
      )}
    >
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        disabled={!hasDetail}
        className={cn(
          'flex w-full items-center gap-2.5 px-4 py-2.5 text-left',
          hasDetail ? 'cursor-pointer hover:bg-accent/60' : 'cursor-default',
        )}
      >
        <Terminal className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="shrink-0 font-mono text-xs font-semibold text-muted-foreground">
          {item.name}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
          {briefArgs(item.name, item.args)}
        </span>
        <StatusBadge item={item} />
        {hasDetail && (
          <ChevronDown
            className={cn(
              'size-3.5 shrink-0 text-muted-foreground transition-transform',
              expanded && 'rotate-180',
            )}
          />
        )}
      </button>
      {expanded && (
        <div className="space-y-2 border-t px-4 py-2.5">
          {Object.keys(item.args).length > 0 && (
            <pre className="max-h-56 overflow-auto whitespace-pre-wrap rounded-lg bg-muted/50 p-2.5 font-mono text-xs leading-relaxed text-muted-foreground break-all">
              {JSON.stringify(item.args, null, 2)}
            </pre>
          )}
          {item.resultPreview !== '' && (
            <pre className="max-h-56 overflow-auto whitespace-pre-wrap rounded-lg bg-muted/50 p-2.5 font-mono text-xs leading-relaxed text-muted-foreground/80">
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
