// 侧栏：模仿 DeepSeek Harness 布局。
// 顶部新会话按钮 + 工作区列表（含图标）+ 会话列表（胶囊风格）。
// 上下文用量由输入区环形指示器（ContextRing）展示，侧栏不再重复显示。

import { FolderOpen, MessageSquarePlus, Search } from 'lucide-react'

import { Button } from '@/components/ui/button'
import type { SessionSummary } from '@/lib/types'
import { cn } from '@/lib/utils'

interface SidebarProps {
  sessions: SessionSummary[]
  currentSessionId: string | null
  cwd: string
  running: boolean
  onNewSession: () => void
  onResume: (id: string) => void
}

export function Sidebar({
  sessions,
  currentSessionId,
  cwd,
  running,
  onNewSession,
  onResume,
}: SidebarProps) {
  // 从 cwd 中提取工作区名称
  const workspaceName = cwd ? cwd.split('/').filter(Boolean).pop() ?? '工作区' : '工作区'

  return (
    <div className="flex h-full w-80 shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
      {/* 头部：新会话按钮 */}
      <div className="px-3 pt-3 pb-1">
        <Button
          variant="outline"
          size="sm"
          className="w-full justify-start gap-2 rounded-xl border-dashed text-xs"
          onClick={onNewSession}
        >
          <MessageSquarePlus className="size-3.5" />
          新会话
        </Button>
      </div>

      {/* 工作区区域 */}
      <div className="px-3 pt-2 pb-1">
        <div className="flex items-center justify-between px-1 pb-1.5">
          <span className="text-[10px] font-medium text-muted-foreground">工作区</span>
          <div className="flex items-center gap-0.5">
            <button
              type="button"
              className="flex size-5 items-center justify-center rounded text-muted-foreground/60 hover:text-muted-foreground"
              title="搜索"
            >
              <Search className="size-3" />
            </button>
          </div>
        </div>
        <div className="flex items-center gap-2 rounded-lg bg-sidebar-accent/50 px-2.5 py-1.5 text-xs">
          <FolderOpen className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate font-medium">{workspaceName}</span>
        </div>
      </div>

      {/* 会话列表 */}
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pt-1">
        <div className="space-y-1 pb-3">
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
                  'flex w-full min-w-0 items-center gap-2 rounded-lg px-2.5 py-2 text-left text-sm transition-colors',
                  active
                    ? 'bg-sidebar-primary/10 font-medium text-sidebar-primary'
                    : 'text-sidebar-foreground hover:bg-sidebar-accent/50',
                )}
              >
                <span
                  className="block min-w-0 flex-1 overflow-hidden text-ellipsis whitespace-nowrap"
                  title={title}
                >
                  {title}
                </span>
                {active && running && (
                  <span className="relative flex size-1.5 shrink-0" aria-hidden="true">
                    <span className="absolute inline-flex size-full animate-ping rounded-full bg-primary opacity-75" />
                    <span className="relative inline-flex size-1.5 rounded-full bg-primary" />
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
    </div>
  )
}