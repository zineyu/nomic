// 侧栏：模仿 DeepSeek Harness 布局。
// 宽度由 App 容器统一控制（桌面 320px，移动端抽屉 max-w-[85vw]）。
// 按 workspace 分组（可折叠）的会话列表，工作区组标题采用卡片样式。
// 组标题右侧带「新建会话」按钮（在该 workspace 下创建）；「工作区」标题行带
// 「添加工作区」按钮，展开内联输入框登记新 workspace（可无任何会话）。
// 展开的会话列表缩进在组标题下方，并带竖向引导线，体现 session 对 workspace 的从属；
// 折叠组之间保持紧凑间距，展开的组以额外下边距分隔。
// 上下文用量由输入区环形指示器（ContextRing）展示，侧栏不再重复显示。
// 无默认 workspace：新会话必须归属明确的 workspace（组标题按钮或启动页选择栏）。

import { ChevronRight, FolderOpen, FolderPlus, Plus, Search } from 'lucide-react'
import { useId, useState } from 'react'

import { groupSessionsWithWorkspaces } from '@/lib/sessions'
import type { SessionSummary, WorkspaceSummary } from '@/lib/types'
import { cn } from '@/lib/utils'

interface SidebarProps {
  sessions: SessionSummary[]
  /** 已登记的全部 workspace（含无会话的；为空时分组退化为纯会话视图） */
  workspaces: WorkspaceSummary[]
  currentSessionId: string | null
  workspace: string
  running: boolean
  /** 新建会话（归属指定 workspace 目录；无默认 workspace，必须显式指定） */
  onNewSession: (workspace: string) => void
  /** 登记新 workspace；失败时抛出错误消息（就地展示在输入框下方） */
  onAddWorkspace: (path: string) => Promise<void>
  onResume: (id: string) => void
}

export function Sidebar({
  sessions,
  workspaces,
  currentSessionId,
  workspace,
  running,
  onNewSession,
  onAddWorkspace,
  onResume,
}: SidebarProps) {
  const groups = groupSessionsWithWorkspaces(workspaces, sessions)
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

  // 「添加工作区」内联输入：展开状态 + 输入值 + 提交中 + 就地错误
  const [adding, setAdding] = useState(false)
  const [newPath, setNewPath] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [addError, setAddError] = useState<string | null>(null)
  const closeAddInput = () => {
    setAdding(false)
    setNewPath('')
    setAddError(null)
  }
  const submitWorkspace = async () => {
    const path = newPath.trim()
    if (!path || submitting) return
    setSubmitting(true)
    setAddError(null)
    try {
      await onAddWorkspace(path)
      closeAddInput()
    } catch (error) {
      setAddError(error instanceof Error ? error.message : String(error))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="flex h-full w-full shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
      {/* 工作区区域标题（实际工作区在下方会话列表中以卡片样式分组展示） */}
      <div className="px-3 pt-3 pb-1">
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
            <button
              type="button"
              aria-label="添加工作区"
              aria-expanded={adding}
              className="flex size-5 items-center justify-center rounded text-muted-foreground/60 hover:text-muted-foreground"
              title="添加工作区"
              onClick={() => (adding ? closeAddInput() : setAdding(true))}
            >
              <FolderPlus className="size-3.5" />
            </button>
          </div>
        </div>
        {/* 添加工作区：内联路径输入（回车提交，Esc 取消） */}
        {adding && (
          <div className="px-1 pb-1.5">
            <input
              type="text"
              value={newPath}
              autoFocus
              disabled={submitting}
              placeholder="目录路径，如 ~/code/proj"
              aria-label="工作区路径"
              aria-invalid={addError !== null}
              className="w-full rounded-md border border-input bg-background px-2 py-1 text-xs outline-none placeholder:text-muted-foreground/60 focus:border-ring disabled:opacity-60"
              onChange={(e) => setNewPath(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void submitWorkspace()
                else if (e.key === 'Escape') closeAddInput()
              }}
            />
            {addError && (
              <p role="alert" className="mt-1 text-xs text-destructive">
                {addError}
              </p>
            )}
          </div>
        )}
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
                className={cn('group', !isCollapsed && 'mb-2')}
              >
                <h3 className="flex items-center pb-1 text-xs font-medium text-muted-foreground">
                  <button
                    type="button"
                    aria-expanded={!isCollapsed}
                    aria-controls={listId}
                    onClick={() => toggleGroup(group.workspace)}
                    title={group.workspace}
                    className={cn(
                      'flex min-w-0 flex-1 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left transition-colors',
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
                      <span className="size-1.5 shrink-0 rounded-full bg-foreground" aria-hidden="true" />
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
                  {/* 在该 workspace 下新建会话（悬停组标题时显现） */}
                  <button
                    type="button"
                    aria-label={`在 ${group.workspace} 下新建会话`}
                    title={`在 ${group.workspace} 下新建会话`}
                    onClick={() => onNewSession(group.workspace)}
                    className="ml-1 flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground/60 opacity-0 transition-opacity hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:opacity-100 group-hover:opacity-100"
                  >
                    <Plus className="size-3" aria-hidden="true" />
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
                              ? 'bg-sidebar-accent font-medium text-sidebar-foreground'
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
                              <span className="absolute inline-flex size-full animate-ping rounded-full bg-foreground opacity-75" />
                              <span className="relative inline-flex size-1.5 rounded-full bg-foreground" />
                            </span>
                          )}
                        </button>
                      )
                    })}
                    {group.sessions.length === 0 && (
                      <div className="px-2.5 py-1 text-xs text-muted-foreground/60">
                        暂无会话
                      </div>
                    )}
                  </div>
                )}
              </section>
            )
          })}
          {groups.length === 0 && (
            <div className="px-3 py-8 text-center text-xs text-muted-foreground">
              还没有会话记录
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
