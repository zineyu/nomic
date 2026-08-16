// 侧栏：会话列表（新建/恢复）+ 模型选择器 + 工作目录。

import { useEffect, useState } from 'react'
import { MessageSquarePlus, MessagesSquare } from 'lucide-react'

import { ModelPicker } from '@/components/ModelPicker'
import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Separator } from '@/components/ui/separator'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { api } from '@/lib/api'
import type { SessionSummary } from '@/lib/types'
import { cn } from '@/lib/utils'

interface SidebarProps {
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
  const sameDay = date.toDateString() === now.toDateString()
  if (sameDay) {
    return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
  }
  return date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' })
}

export function Sidebar({
  currentSessionId,
  modelSpec,
  reasoning,
  cwd,
  onNewSession,
  onResume,
  onModelChanged,
}: SidebarProps) {
  const [sessions, setSessions] = useState<SessionSummary[]>([])

  useEffect(() => {
    void api.sessions().then(setSessions).catch(() => {})
  }, [currentSessionId])

  return (
    <div className="flex h-full w-64 shrink-0 flex-col border-r bg-sidebar text-sidebar-foreground">
      <div className="flex items-center justify-between px-3 py-3">
        <div className="flex items-center gap-2">
          <span className="text-base">🦀</span>
          <span className="text-sm font-semibold tracking-tight">nomic</span>
        </div>
        <Tooltip>
          <TooltipTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="size-7"
              onClick={onNewSession}
              title="新对话"
            >
              <MessageSquarePlus className="size-4" />
            </Button>
          </TooltipTrigger>
          <TooltipContent>新对话</TooltipContent>
        </Tooltip>
      </div>

      <Separator />

      <div className="flex items-center gap-1 px-2 py-1.5">
        <ModelPicker
          currentSpec={modelSpec}
          reasoning={reasoning}
          onChanged={onModelChanged}
        />
      </div>

      <Separator />

      <ScrollArea className="min-h-0 flex-1">
        <div className="space-y-0.5 p-2">
          {sessions.map((session) => (
            <button
              key={session.id}
              type="button"
              onClick={() => onResume(session.id)}
              className={cn(
                'flex w-full flex-col gap-0.5 rounded-md px-2.5 py-2 text-left transition-colors',
                session.id === currentSessionId
                  ? 'bg-accent text-accent-foreground'
                  : 'hover:bg-accent/60',
              )}
            >
              <div className="flex items-center gap-1.5 text-xs">
                <MessagesSquare className="size-3 shrink-0 opacity-60" />
                <span className="min-w-0 flex-1 truncate font-medium">
                  {session.title ?? '新会话'}
                </span>
              </div>
              <div className="pl-5 text-[10px] text-muted-foreground">
                {formatTime(session.last_message_at)}
                {session.message_count > 0 && ` · ${session.message_count} 条`}
              </div>
            </button>
          ))}
          {sessions.length === 0 && (
            <div className="px-3 py-4 text-xs text-muted-foreground">
              还没有会话记录
            </div>
          )}
        </div>
      </ScrollArea>

      <Separator />
      <div className="truncate px-3 py-2 text-[10px] text-muted-foreground" title={cwd}>
        {cwd || '—'}
      </div>
    </div>
  )
}
