// 侧栏：简洁胶囊风格会话列表。
//
// 参考截图：每个 session 用一个圆角胶囊展示标题；当前项用主色背景高亮，
// 非当前项透明背景悬停变色；只展示标题，不展示时间/消息数分组。

import { Folder, MessageSquarePlus } from 'lucide-react'

import { Button } from '@/components/ui/button'
import type { SessionSummary } from '@/lib/types'
import { cn } from '@/lib/utils'

interface SidebarProps {
  sessions: SessionSummary[]
  currentSessionId: string | null
  cwd: string
  contextTokens: number
  contextWindow: number | null
  running: boolean
  onNewSession: () => void
  onResume: (id: string) => void
}

export function Sidebar({
  sessions,
  currentSessionId,
  cwd,
  contextTokens,
  contextWindow,
  running,
  onNewSession,
  onResume,
}: SidebarProps) {
  const usage = contextWindow && contextWindow > 0 ? contextTokens / contextWindow : null
  const pct = usage !== null ? Math.min(usage * 100, 100) : null

  return (
    <div className="flex h-full w-80 shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
      {/* 头部：标题 + 新对话 */}
      <div className="flex items-center justify-between px-4 pt-4 pb-2">
        <h1 className="text-sm font-semibold">会话</h1>
        <Button
          variant="ghost"
          size="icon"
          className="size-7 rounded-lg text-muted-foreground hover:bg-sidebar-accent hover:text-sidebar-foreground"
          onClick={onNewSession}
          title="新对话"
        >
          <MessageSquarePlus className="size-4" />
        </Button>
      </div>

      {/* 会话列表：胶囊风格 */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="space-y-2 px-3 pb-3">
          {sessions.map((session) => {
            const active = session.id === currentSessionId
            const title = session.title ?? '新会话'
            return (
              <button
                key={session.id}
                type="button"
                onClick={() => onResume(session.id)}
                aria-current={active ? 'page' : undefined}
                title={title}
                className={cn(
                  'flex w-full min-w-0 items-center gap-2 rounded-2xl px-3.5 py-2.5 text-left text-sm transition-colors',
                  active
                    ? 'bg-sidebar-primary font-medium text-sidebar-primary-foreground'
                    : 'bg-muted text-sidebar-foreground hover:bg-sidebar-accent',
                )}
              >
                <span
                  className="block min-w-0 flex-1 overflow-hidden text-ellipsis whitespace-nowrap"
                  title={title}
                >
                  {title}
                </span>
                {active && running && (
                  <span className="relative flex size-2 shrink-0" aria-hidden="true">
                    <span className="absolute inline-flex size-full animate-ping rounded-full bg-sidebar-primary-foreground/70 opacity-75" />
                    <span className="relative inline-flex size-2 rounded-full bg-sidebar-primary-foreground" />
                  </span>
                )}
              </button>
            )
          })}
          {sessions.length === 0 && (
            <div className="px-3 py-8 text-center text-xs text-muted-foreground">
              还没有会话记录
            </div>
          )}
        </div>
      </div>

      {/* 底部：工作目录 + 上下文用量 */}
      <div className="border-t border-sidebar-border px-4 py-3">
        <div
          className="mb-2.5 flex items-center gap-1.5 font-mono text-xs text-muted-foreground"
          title={cwd}
        >
          <Folder className="size-3 shrink-0 opacity-70" />
          <span className="truncate">{cwd || '—'}</span>
        </div>
        <div className="flex items-baseline font-mono text-xs text-muted-foreground">
          <span>上下文：{contextTokens.toLocaleString()} tokens</span>
          {pct !== null && (
            <span className="ml-auto font-medium text-sidebar-foreground">
              {pct.toFixed(0)}%
            </span>
          )}
        </div>
        {pct !== null && (
          <div className="mt-1.5 h-1 overflow-hidden rounded-full bg-sidebar-border">
            <div
              className={cn(
                'h-full rounded-full transition-all',
                pct > 80 ? 'bg-destructive' : 'bg-sidebar-primary',
              )}
              style={{ width: `${pct}%` }}
            />
          </div>
        )}
      </div>
    </div>
  )
}
