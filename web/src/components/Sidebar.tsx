// 侧栏：模仿 DeepSeek Harness 布局。
// 顶部新会话按钮 + 按 workspace 分组（可折叠）的会话列表，工作区组标题采用卡片样式。
// 展开的会话列表缩进在组标题下方，并带竖向引导线，体现 session 对 workspace 的从属；
// 折叠组之间保持紧凑间距，展开的组以额外下边距分隔。
// 上下文用量由输入区环形指示器（ContextRing）展示，侧栏不再重复显示。

import { ChevronRight, FolderOpen, MessageSquarePlus, Search } from 'lucide-react'
import { useId, useState } from 'react'

import { Button } from '@/components/ui/button'
import { groupSessionsByWorkspace } from '@/lib/sessions'
import type { SessionSummary } from '@/lib/types'
import { cn } from '@/lib/utils'

interface SidebarProps {
  sessions: SessionSummary[]
  currentSessionId: string | null
  workspace: string
  running: boolean
  onNewSession: () => void
  onResume: (id: string) => void
}

export function Sidebar({
  sessions,
  currentSessionId,
  workspace,
  running,
  onNewSession,
  onResume,
}: SidebarProps) {
  const groups = groupSessionsByWorkspace(sessions)
  const listIdPrefix = useId()
  // 折叠态为本地 UI 状态：记录被折叠的 workspace，新出现的组默认展开
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set())
  const toggleGroup = (key: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })

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

      {/* 工作区区域标题（实际工作区在下方会话列表中以卡片样式分组展示） */}
      <div className="px-3 pt-2 pb-1">
        <div className="flex items-center justify-between px-1 pb-1.5">
          <span className="text-xs font-medium text-muted-foreground">工作区</span>
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
      </div>

      {/* 会话列表（按 workspace 分组，组标题点击折叠/展开）
          间距按折叠态区分：折叠组间紧凑（space-y-1），展开的组用
          mb-2 补出分组边界；展开的会话列表带缩进与竖向引导线，
          视觉上一眼看出 session 从属于哪个 workspace */}
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pt-1">
        <div className="space-y-1 pb-3">
          {groups.map((group, index) => {
            const isCurrent = group.workspace === workspace
            const isCollapsed = collapsed.has(group.workspace)
            const hasActive = group.sessions.some((s) => s.id === currentSessionId)
            const listId = `${listIdPrefix}-group-${index}`
            return (
              <section
                key={group.workspace}
                aria-label={group.workspace}
                className={cn(!isCollapsed && 'mb-2')}
              >
                <h3 className="pb-1 text-xs font-medium text-muted-foreground">
                  <button
                    type="button"
                    aria-expanded={!isCollapsed}
                    aria-controls={listId}
                    onClick={() => toggleGroup(group.workspace)}
                    title={group.workspace}
                    className={cn(
                      'flex w-full min-w-0 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left transition-colors',
                      isCurrent
                        ? 'bg-sidebar-accent text-sidebar-foreground'
                        : 'bg-sidebar-accent/50 hover:bg-sidebar-accent hover:text-sidebar-foreground',
                    )}
                  >
                    <ChevronRight
                      className={cn(
                        'size-3 shrink-0 transition-transform',
                        !isCollapsed && 'rotate-90',
                      )}
                      aria-hidden="true"
                    />
                    <FolderOpen className="size-3.5 shrink-0" aria-hidden="true" />
                    <span className="min-w-0 flex-1 truncate">{group.name}</span>
                    {/* 折叠时隐藏当前会话无从感知，标题上保留活跃指示点 */}
                    {isCollapsed && hasActive && (
                      <span className="size-1.5 shrink-0 rounded-full bg-primary" aria-hidden="true" />
                    )}
                    <span className="shrink-0 tabular-nums text-muted-foreground/70">
                      {group.sessions.length}
                    </span>
                    {isCurrent && (
                      <span className="shrink-0 rounded-full border border-border/60 bg-background/40 px-1.5 py-px text-[10px] leading-4 text-muted-foreground">
                        当前
                      </span>
                    )}
                  </button>
                </h3>
                {!isCollapsed && (
                  <div
                    id={listId}
                    className="mt-1 ml-4 space-y-0.5 border-l border-sidebar-border/70 pl-2"
                  >
                    {group.sessions.map((session) => {
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
                            'flex w-full min-w-0 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left text-sm transition-colors',
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
                  </div>
                )}
              </section>
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