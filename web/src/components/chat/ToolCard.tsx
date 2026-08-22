// 工具执行卡片：终端图标 + 工具名 + 参数摘要 + 状态徽章（✓ 完成 /
// ● 运行中 / ✗ 失败），点击展开完整参数与结果预览。

import { memo, useState } from 'react'
import { ChevronDown, Loader2, Terminal } from 'lucide-react'

import { useCollapse } from '@/lib/anim'
import { cn } from '@/lib/utils'
import { briefArgs } from '@/lib/toolArgs'
import { toolCategoryIconClass } from '@/lib/toolCategory'
import type { ToolItem } from '@/lib/chat'

function StatusBadge({ item }: { item: ToolItem }) {
  if (item.status === 'running') {
    return (
      <span className="inline-flex h-5 shrink-0 items-center gap-1.5 rounded-full px-2 text-xs text-muted-foreground">
        <Loader2 className="size-2.5 animate-spin" />
        运行中
      </span>
    )
  }
  if (item.status === 'error') {
    return (
      <span className="flex h-5 shrink-0 items-center rounded-full px-2 text-xs text-destructive">
        ✗ 失败
      </span>
    )
  }
  return (
    <span className="flex h-5 shrink-0 items-center rounded-full px-2 text-xs text-muted-foreground">
      ✓ 完成
    </span>
  )
}

function ToolCardImpl({ item }: { item: ToolItem }) {
  const [expanded, setExpanded] = useState(false)
  const hasDetail = Object.keys(item.args).length > 0 || item.resultPreview !== ''
  // 展开/收起时做高度动画；收起动画结束后卸载内容
  const { ref: detailRef, mounted: detailMounted } = useCollapse<HTMLDivElement>(expanded)

  return (
    <div className="text-muted-foreground">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        disabled={!hasDetail}
        className={cn(
          'flex h-6 w-full items-center gap-1.5 text-left text-xs',
          hasDetail ? 'cursor-pointer hover:bg-accent/60' : 'cursor-default',
        )}
      >
        <Terminal className={cn('size-3 shrink-0', toolCategoryIconClass(item.name))} />
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
              'size-3 shrink-0 text-muted-foreground transition-transform',
              expanded && 'rotate-180',
            )}
          />
        )}
      </button>
      <div ref={detailRef} className="overflow-hidden">
        {detailMounted && (
          <div className="space-y-1 py-1.5">
            {Object.keys(item.args).length > 0 && (
              <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded bg-muted/30 p-2 font-mono text-xs leading-relaxed text-muted-foreground break-all">
                {JSON.stringify(item.args, null, 2)}
              </pre>
            )}
            {item.resultPreview !== '' && (
              <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded bg-muted/30 p-2 font-mono text-xs leading-relaxed text-muted-foreground/80">
                {item.resultPreview}
              </pre>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

// memo：工具卡片状态在单次执行周期内不频繁变化，避免随流式文本重渲染。
export const ToolCard = memo(ToolCardImpl)
