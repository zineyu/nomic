// 状态栏：上下文 token 用量 + 会话标题。

import { cn } from '@/lib/utils'

interface StatusBarProps {
  contextTokens: number
  contextWindow: number | null
  sessionTitle: string | null
}

export function StatusBar({ contextTokens, contextWindow, sessionTitle }: StatusBarProps) {
  const usage = contextWindow && contextWindow > 0 ? contextTokens / contextWindow : null
  const pct = usage !== null ? Math.min(usage * 100, 100) : null

  return (
    <div className="flex h-8 shrink-0 items-center gap-3 border-t bg-muted/30 px-4 text-xs text-muted-foreground">
      <span>上下文：{contextTokens.toLocaleString()} tokens</span>
      {pct !== null && (
        <div className="flex items-center gap-1.5">
          <div className="h-1 w-16 overflow-hidden rounded-full bg-muted">
            <div
              className={cn(
                'h-full rounded-full transition-all',
                pct > 80 ? 'bg-destructive' : 'bg-primary',
              )}
              style={{ width: `${pct}%` }}
            />
          </div>
          <span className="tabular-nums">{pct.toFixed(0)}%</span>
        </div>
      )}
      {sessionTitle && (
        <span className="ml-auto max-w-64 truncate" title={sessionTitle}>
          {sessionTitle}
        </span>
      )}
    </div>
  )
}
