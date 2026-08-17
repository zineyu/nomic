// 侧栏：会话列表（按今天/本周/更早分组）+ 底部工作目录与
// 上下文用量。布局参考 Kimi 风格：头部计数、分组标签、活动项左侧色条。

import { Folder, MessageSquarePlus, MessagesSquare } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
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

const DAY = 24 * 60 * 60 * 1000

function startOfDay(millis: number): number {
  const d = new Date(millis)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

function formatTime(millis: number | null): string {
  if (!millis) return ''
  const date = new Date(millis)
  const now = new Date()
  const diffMs = now.getTime() - millis
  const minute = 60_000
  const hour = 60 * minute
  if (diffMs < minute) return '刚刚'
  if (diffMs < hour) return `${Math.floor(diffMs / minute)} 分钟前`
  if (startOfDay(now.getTime()) === startOfDay(millis)) {
    return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
  }
  if (diffMs < 7 * DAY) {
    return `周${'日一二三四五六'[date.getDay()]}`
  }
  if (date.getFullYear() === now.getFullYear()) {
    return date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' })
  }
  return date.toLocaleDateString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  })
}

/** 按时间分组：今天 / 本周 / 更早（sessions 已按 last_message_at 降序）。 */
function groupSessions(sessions: SessionSummary[]): { label: string; items: SessionSummary[] }[] {
  const groups: { label: string; items: SessionSummary[] }[] = []
  for (const session of sessions) {
    const at = session.last_message_at ?? Date.now()
    let label = '更早'
    if (startOfDay(at) === startOfDay(Date.now())) {
      label = '今天'
    } else if (Date.now() - at < 7 * DAY) {
      label = '本周'
    }
    let group = groups[groups.length - 1]
    if (!group || group.label !== label) {
      group = { label, items: [] }
      groups.push(group)
    }
    group.items.push(session)
  }
  return groups
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
  const groups = groupSessions(sessions)
  const usage = contextWindow && contextWindow > 0 ? contextTokens / contextWindow : null
  const pct = usage !== null ? Math.min(usage * 100, 100) : null

  return (
    <div className="flex h-full w-64 shrink-0 flex-col bg-sidebar text-sidebar-foreground">
      {/* 头部：标题 + 会话计数 */}
      <div className="flex items-center gap-2 px-4 pt-4 pb-1">
        <h1 className="text-sm font-semibold">会话</h1>
        {sessions.length > 0 && (
          <span className="rounded-full border border-sidebar-border bg-background px-2 py-px text-xs tabular-nums text-muted-foreground">
            {sessions.length}
          </span>
        )}
      </div>

      <div className="px-3 py-3">
        <Button
          variant="outline"
          className="h-8 w-full justify-start gap-2 rounded-lg border-sidebar-border bg-transparent px-3.5 text-xs font-normal hover:border-sidebar-ring hover:bg-sidebar-accent"
          onClick={onNewSession}
        >
          <MessageSquarePlus className="size-3" />
          新对话
        </Button>
      </div>

      {/* 会话列表（按时间分组） */}
      <ScrollArea className="min-h-0 flex-1">
        <div className="space-y-3 px-2 pb-2">
          {groups.map((group) => (
            <div key={group.label}>
              <div className="mb-1 px-2.5 text-xs font-medium tracking-[0.1em] text-muted-foreground">
                {group.label}
              </div>
              <div className="space-y-1">
                {group.items.map((session) => {
                  const active = session.id === currentSessionId
                  const meta = active
                    ? `${session.message_count} 条消息 · ${contextTokens.toLocaleString()} tokens`
                    : `${session.message_count} 条消息 · ${formatTime(session.last_message_at)}`
                  return (
                    <button
                      key={session.id}
                      type="button"
                      onClick={() => onResume(session.id)}
                      aria-current={active ? 'page' : undefined}
                      className={cn(
                        'relative flex w-full flex-col gap-1 rounded-lg border border-transparent px-3.5 py-2.5 text-left transition-colors',
                        active
                          ? 'border-sidebar-border bg-sidebar-accent'
                          : 'hover:bg-sidebar-accent/60',
                      )}
                    >
                      {active && (
                        <span className="absolute top-1.5 bottom-1.5 left-0 w-[3px] rounded-r-full bg-sidebar-ring" />
                      )}
                      <span
                        className={cn(
                          'truncate text-sm',
                          active ? 'font-semibold text-sidebar-foreground' : 'text-muted-foreground',
                        )}
                      >
                        {session.title ?? '新会话'}
                      </span>
                      <span className="text-xs text-muted-foreground">{meta}</span>
                      {active && running && (
                        <span
                          className="absolute top-1/2 right-3.5 flex size-3.5 -translate-y-1/2 items-center justify-center rounded-full border border-success/50"
                          title="运行中"
                        >
                          <span className="size-1.5 rounded-full bg-success" />
                        </span>
                      )}
                    </button>
                  )
                })}
              </div>
            </div>
          ))}
          {sessions.length === 0 && (
            <div className="flex flex-col items-center gap-2 px-3 py-8 text-muted-foreground">
              <MessagesSquare className="size-5 opacity-40" />
              <span className="text-xs">还没有会话记录</span>
            </div>
          )}
        </div>
      </ScrollArea>

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
