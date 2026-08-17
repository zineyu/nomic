// 侧栏：会话列表（新建/恢复）+ 模型选择器 + 工作目录。会话列表由 useChat 提供。

import { Folder, MessageSquarePlus, MessagesSquare, Moon, Monitor, Sun } from 'lucide-react'

import { ModelPicker } from '@/components/ModelPicker'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Separator } from '@/components/ui/separator'
import { useTheme } from '@/hooks/useTheme'
import type { SessionSummary } from '@/lib/types'
import { cn } from '@/lib/utils'

interface SidebarProps {
  sessions: SessionSummary[]
  currentSessionId: string | null
  modelSpec: string | null
  reasoning: string | null
  cwd: string
  onNewSession: () => void
  onResume: (id: string) => void
  onModelChanged: () => void
}

function formatTime(millis: number | null): string {
  if (!millis) return ''
  const date = new Date(millis)
  const now = new Date()
  const diffMs = now.getTime() - millis
  const minute = 60_000
  const hour = 60 * minute
  const day = 24 * hour
  if (diffMs < minute) return '刚刚'
  if (diffMs < hour) return `${Math.floor(diffMs / minute)} 分钟前`
  if (date.toDateString() === now.toDateString()) {
    return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
  }
  const yesterday = new Date(now.getTime() - day)
  if (date.toDateString() === yesterday.toDateString()) return '昨天'
  if (date.getFullYear() === now.getFullYear()) {
    return date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' })
  }
  return date.toLocaleDateString('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
  })
}

export function Sidebar({
  sessions,
  currentSessionId,
  modelSpec,
  reasoning,
  cwd,
  onNewSession,
  onResume,
  onModelChanged,
}: SidebarProps) {
  const { theme, cycle } = useTheme()

  const ThemeIcon = theme === 'dark' ? Moon : theme === 'light' ? Sun : Monitor

  return (
    <div className="flex h-full w-64 shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground">
      <div className="flex items-center gap-2.5 px-4 py-3">
        <img
          src="/favicon.svg"
          alt="nomic"
          className="size-7 shrink-0 rounded-lg shadow-sm"
        />
        <span className="text-sm font-semibold tracking-tight">nomic</span>
        <Badge variant="secondary" className="h-5 px-1.5 text-xs font-medium">
          web
        </Badge>
      </div>

      <div className="px-3 pb-1.5">
        <Button
          variant="outline"
          size="sm"
          className="h-8 w-full justify-start gap-2 rounded-lg border-sidebar-border bg-transparent text-xs font-medium hover:bg-sidebar-accent"
          onClick={onNewSession}
        >
          <MessageSquarePlus className="size-3.5" />
          新对话
        </Button>
      </div>

      <Separator />

      <div className="px-4 py-2">
        <div className="mb-1.5 text-xs font-medium text-muted-foreground">模型</div>
        <ModelPicker
          currentSpec={modelSpec}
          reasoning={reasoning}
          onChanged={onModelChanged}
        />
      </div>

      <Separator />

      <div className="flex items-center justify-between px-4 pb-1.5 pt-2.5">
        <div className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
          <MessagesSquare className="size-3" />
          会话
        </div>
        {sessions.length > 0 && (
          <span className="text-xs tabular-nums text-muted-foreground">
            {sessions.length}
          </span>
        )}
      </div>

      <ScrollArea className="min-h-0 flex-1">
        <div className="space-y-0.5 px-2 pb-2">
          {sessions.map((session) => {
            const active = session.id === currentSessionId
            return (
              <button
                key={session.id}
                type="button"
                onClick={() => onResume(session.id)}
                aria-current={active ? 'page' : undefined}
                className={cn(
                  'relative flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left transition-colors',
                  active ? 'bg-accent text-accent-foreground' : 'hover:bg-sidebar-accent/70',
                )}
              >
                {active && (
                  <span className="absolute top-1/2 left-0 h-4 w-[3px] -translate-y-1/2 rounded-r-full bg-current opacity-90" />
                )}
                <MessagesSquare
                  className={cn('size-3.5 shrink-0', active ? 'opacity-80' : 'opacity-50')}
                />
                <span className="min-w-0 flex-1">
                  <span className="flex items-baseline justify-between gap-2">
                    <span
                      className={cn(
                        'truncate text-xs',
                        active ? 'font-semibold' : 'font-medium',
                      )}
                    >
                      {session.title ?? '新会话'}
                    </span>
                    <span
                      className={cn(
                        'shrink-0 text-xs tabular-nums',
                        active ? 'opacity-70' : 'text-muted-foreground',
                      )}
                    >
                      {formatTime(session.last_message_at)}
                    </span>
                  </span>
                  {session.message_count > 0 && (
                    <span
                      className={cn(
                        'block text-xs',
                        active ? 'opacity-70' : 'text-muted-foreground',
                      )}
                    >
                      {session.message_count} 条消息
                    </span>
                  )}
                </span>
              </button>
            )
          })}
          {sessions.length === 0 && (
            <div className="flex flex-col items-center gap-2 px-3 py-8 text-muted-foreground">
              <MessagesSquare className="size-5 opacity-40" />
              <span className="text-xs">还没有会话记录</span>
            </div>
          )}
        </div>
      </ScrollArea>

      <Separator />
      <div className="flex items-center gap-2 px-4 py-2.5 text-xs text-muted-foreground">
        <Folder className="size-3.5 shrink-0 opacity-70" />
        <span className="min-w-0 flex-1 truncate" title={cwd}>
          {cwd || '—'}
        </span>
        <Button
          variant="ghost"
          size="icon"
          className="size-6 shrink-0"
          onClick={cycle}
          title={`主题：${theme === 'light' ? '浅色' : theme === 'dark' ? '深色' : '跟随系统'}`}
        >
          <ThemeIcon className="size-3.5" />
        </Button>
      </div>
    </div>
  )
}
