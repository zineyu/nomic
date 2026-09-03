// 侧栏：模仿 DeepSeek Harness 布局。
// 宽度由 App 容器统一控制（桌面 320px，移动端抽屉 max-w-[85vw]）。
// 按 project 分组（可折叠）的 work 列表（ADR-0044：work 为一等入口，
// 点击打开其主 session），项目组标题采用卡片样式。
// 组标题右侧带「新建会话」按钮（在该 project 下创建 work）；「项目」标题行带
// 「添加项目」按钮，展开内联输入框登记新 project（可无任何 work）。
// work 行悬停显露「重命名」（内联编辑，空白提交 = 清除自定义标题回退派生）
// 与「删除」（级联删除名下全部 session 不可恢复，弹确认对话框）操作。
// 展开的 work 列表缩进在组标题下方，并带竖向引导线，体现 work 对 project 的从属；
// 折叠组之间保持紧凑间距，展开的组以额外下边距分隔。
// 上下文用量由输入区环形指示器（ContextRing）展示，侧栏不再重复显示。
// 无默认 project：新建必须归属明确的 project（组标题按钮或启动页选择栏）。

import { ChevronRight, FolderOpen, FolderPlus, Pencil, Plus, Search, Trash2 } from 'lucide-react'
import { useId, useState } from 'react'

import { groupWorksWithProjects } from '@/lib/works'
import type { WorkSummary, ProjectSummary } from '@/lib/types'
import { cn } from '@/lib/utils'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

interface SidebarProps {
  works: WorkSummary[]
  /** 已登记的全部 project（含无 work 的；为空时分组退化为纯 work 视图） */
  projects: ProjectSummary[]
  currentSessionId: string | null
  running: boolean
  /** 新建 work（归属指定 project 目录；无默认 project，必须显式指定） */
  onNewSession: (project: string) => void
  /** 登记新 project；失败时抛出错误消息（就地展示在输入框下方） */
  onAddProject: (path: string) => Promise<void>
  /** 重命名 work（空白标题 = 清除自定义，回退派生标题）；失败时抛出错误消息 */
  onRenameWork: (id: string, title: string) => Promise<void>
  /** 删除 work（级联删除名下全部 session，不可恢复）；失败时抛出错误消息 */
  onDeleteWork: (id: string) => Promise<void>
  /** 删除 project；`force` 级联删除名下全部 work 与 session；失败时抛出错误消息 */
  onDeleteProject: (id: string, force: boolean) => Promise<void>
  onResume: (id: string) => void
}

/** 删除确认目标（会话 / 项目共用一个确认对话框）。 */
type ConfirmTarget =
  | { kind: 'work'; id: string; title: string }
  | { kind: 'project'; id: string; name: string; path: string; sessionCount: number }

/** 行内操作按钮样式（悬停行时由父级 group/item 控制显现）。 */
const rowActionClass =
  'flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground/60 transition-all outline-none hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:ring-2 focus-visible:ring-ring/50 active:scale-90'

export function Sidebar({
  works,
  projects,
  currentSessionId,
  running,
  onNewSession,
  onAddProject,
  onRenameWork,
  onDeleteWork,
  onDeleteProject,
  onResume,
}: SidebarProps) {
  const groups = groupWorksWithProjects(projects, works)
  const listIdPrefix = useId()
  // 折叠态为本地 UI 状态：记录被折叠的 project，新出现的组默认展开
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set())
  const toggleGroup = (key: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })

  // 「添加项目」内联输入：展开状态 + 输入值 + 提交中 + 就地错误
  const [adding, setAdding] = useState(false)
  const [newPath, setNewPath] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [addError, setAddError] = useState<string | null>(null)
  const closeAddInput = () => {
    setAdding(false)
    setNewPath('')
    setAddError(null)
  }
  const submitProject = async () => {
    const path = newPath.trim()
    if (!path || submitting) return
    setSubmitting(true)
    setAddError(null)
    try {
      await onAddProject(path)
      closeAddInput()
    } catch (error) {
      setAddError(error instanceof Error ? error.message : String(error))
    } finally {
      setSubmitting(false)
    }
  }

  // 会话重命名：内联编辑（Enter 提交 / Esc 或失焦取消），失败就地展示
  const [renaming, setRenaming] = useState<{ id: string; value: string } | null>(null)
  const [renamingSubmitting, setRenamingSubmitting] = useState(false)
  const [renameError, setRenameError] = useState<string | null>(null)
  const closeRename = () => {
    setRenaming(null)
    setRenameError(null)
  }
  const submitRename = async () => {
    if (!renaming || renamingSubmitting) return
    setRenamingSubmitting(true)
    setRenameError(null)
    try {
      await onRenameWork(renaming.id, renaming.value)
      closeRename()
    } catch (error) {
      setRenameError(error instanceof Error ? error.message : String(error))
    } finally {
      setRenamingSubmitting(false)
    }
  }

  // 删除确认：会话与项目共用一个对话框，失败就地展示在对话框内
  const [confirm, setConfirm] = useState<ConfirmTarget | null>(null)
  const [confirming, setConfirming] = useState(false)
  const [confirmError, setConfirmError] = useState<string | null>(null)
  const closeConfirm = () => {
    if (confirming) return
    setConfirm(null)
    setConfirmError(null)
  }
  const submitConfirm = async () => {
    if (!confirm || confirming) return
    setConfirming(true)
    setConfirmError(null)
    try {
      if (confirm.kind === 'work') {
        await onDeleteWork(confirm.id)
      } else {
        await onDeleteProject(confirm.id, confirm.sessionCount > 0)
      }
      setConfirm(null)
    } catch (error) {
      setConfirmError(error instanceof Error ? error.message : String(error))
    } finally {
      setConfirming(false)
    }
  }

  return (
    <div className="flex h-full w-full shrink-0 flex-col border-r border-sidebar-border bg-sidebar text-sidebar-foreground">
      {/* 项目区域标题（实际项目在下方会话列表中以卡片样式分组展示） */}
      <div className="px-3 pt-3 pb-1">
        <div className="flex items-center justify-between px-1 pb-1.5">
          <span className="text-xs font-medium text-muted-foreground">项目</span>
          <div className="flex items-center gap-0.5">
            <button
              type="button"
              className="flex size-5 items-center justify-center rounded text-muted-foreground/60 transition-all outline-none hover:bg-sidebar-accent hover:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring/50 active:scale-90"
              title="搜索"
            >
              <Search className="size-3" />
            </button>
            <button
              type="button"
              aria-label="添加项目"
              aria-expanded={adding}
              className="flex size-5 items-center justify-center rounded text-muted-foreground/60 transition-all outline-none hover:bg-sidebar-accent hover:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring/50 active:scale-90"
              title="添加项目"
              onClick={() => (adding ? closeAddInput() : setAdding(true))}
            >
              <FolderPlus className="size-3.5" />
            </button>
          </div>
        </div>
        {/* 添加项目：内联路径输入（回车提交，Esc 取消） */}
        {adding && (
          <div className="px-1 pb-1.5">
            <input
              type="text"
              value={newPath}
              autoFocus
              disabled={submitting}
              placeholder="目录路径"
              aria-label="项目路径"
              aria-invalid={addError !== null}
              className="w-full rounded-md border border-input bg-background px-2 py-1 text-xs outline-none transition-[color,box-shadow] placeholder:text-muted-foreground/60 focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-60"
              onChange={(e) => setNewPath(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void submitProject()
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

      {/* 会话列表（按 project 分组，组标题点击折叠/展开）
          间距按折叠态区分：折叠组间紧凑（space-y-1），展开的组用
          mb-2 补出分组边界；展开的会话列表带缩进与竖向引导线，
          视觉上一眼看出 session 从属于哪个 project */}
      <div className="min-h-0 flex-1 overflow-y-auto px-3 pt-1">
        <div className="space-y-1 pb-3">
          {groups.map((group, index) => {
            const isCollapsed = collapsed.has(group.project)
            const hasActive = group.works.some((w) => w.main_session_id === currentSessionId)
            const listId = `${listIdPrefix}-group-${index}`
            return (
              <section
                key={group.project}
                aria-label={group.project}
                className={cn('group', !isCollapsed && 'mb-2')}
              >
                <h3 className="flex items-center pb-1 text-xs font-medium text-muted-foreground">
                  <button
                    type="button"
                    aria-expanded={!isCollapsed}
                    aria-controls={listId}
                    onClick={() => toggleGroup(group.project)}
                    title={group.project}
                    className={cn(
                      'flex min-w-0 flex-1 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
                      'bg-sidebar-accent/50 hover:bg-sidebar-accent hover:text-sidebar-foreground',
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
                      {group.works.length}
                    </span>
                  </button>
                  {/* 在该 project 下新建会话（悬停组标题时显现） */}
                  <button
                    type="button"
                    aria-label={`在 ${group.project} 下新建会话`}
                    title={`在 ${group.project} 下新建会话`}
                    onClick={() => onNewSession(group.project)}
                    className="ml-1 flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground/60 opacity-0 transition-all outline-none hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring/50 active:scale-90 group-hover:opacity-100"
                  >
                    <Plus className="size-3" aria-hidden="true" />
                  </button>
                  {/* 删除 project（悬停组标题时显现；含会话时确认对话框提示级联） */}
                  <button
                    type="button"
                    aria-label={`删除项目 ${group.name}`}
                    title={`删除项目 ${group.name}`}
                    onClick={() =>
                      setConfirm({
                        kind: 'project',
                        id: group.projectId,
                        name: group.name,
                        path: group.project,
                        sessionCount: group.works.length,
                      })
                    }
                    className="ml-0.5 flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground/60 opacity-0 transition-all outline-none hover:bg-sidebar-accent hover:text-sidebar-foreground focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring/50 active:scale-90 group-hover:opacity-100"
                  >
                    <Trash2 className="size-3" aria-hidden="true" />
                  </button>
                </h3>
                {!isCollapsed && (
                  <div
                    id={listId}
                    className="mt-1 ml-4 space-y-0.5 border-l border-sidebar-border/70 pl-2"
                  >
                    {group.works.map((work) => {
                      const active = work.main_session_id === currentSessionId
                      const title = work.title ?? '新会话'
                      const renamingThis = renaming?.id === work.id
                      return (
                        <div key={work.id} className="group/item">
                          <div className="flex items-center gap-0.5">
                            {renamingThis ? (
                              <input
                                type="text"
                                value={renaming.value}
                                autoFocus
                                disabled={renamingSubmitting}
                                aria-label="会话标题"
                                aria-invalid={renameError !== null}
                                onFocus={(e) => e.target.select()}
                                className="min-w-0 flex-1 rounded-md border border-input bg-background px-2 py-1 text-xs outline-none transition-[color,box-shadow] placeholder:text-muted-foreground/60 focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-60"
                                onChange={(e) =>
                                  setRenaming({ id: work.id, value: e.target.value })
                                }
                                onKeyDown={(e) => {
                                  if (e.key === 'Enter') void submitRename()
                                  else if (e.key === 'Escape') closeRename()
                                }}
                                onBlur={() => {
                                  if (!renamingSubmitting) closeRename()
                                }}
                              />
                            ) : (
                              <button
                                type="button"
                                onClick={() => onResume(work.main_session_id)}
                                aria-current={active ? 'page' : undefined}
                                title={title}
                                className={cn(
                                  'flex min-w-0 flex-1 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left text-sm transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
                                  active
                                    ? 'bg-sidebar-accent font-medium text-sidebar-foreground'
                                    : 'text-sidebar-foreground hover:bg-sidebar-accent/50 active:bg-sidebar-accent/70',
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
                            )}
                            {/* 行内操作：重命名 / 删除（悬停或键盘聚焦行时显现） */}
                            {!renamingThis && (
                              <div className="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity focus-within:opacity-100 group-hover/item:opacity-100">
                                <button
                                  type="button"
                                  aria-label="重命名会话"
                                  title="重命名会话"
                                  className={rowActionClass}
                                  onClick={() =>
                                    setRenaming({ id: work.id, value: work.title ?? '' })
                                  }
                                >
                                  <Pencil className="size-3" aria-hidden="true" />
                                </button>
                                <button
                                  type="button"
                                  aria-label="删除会话"
                                  title="删除会话"
                                  className={rowActionClass}
                                  onClick={() => setConfirm({ kind: 'work', id: work.id, title })}
                                >
                                  <Trash2 className="size-3" aria-hidden="true" />
                                </button>
                              </div>
                            )}
                          </div>
                          {renamingThis && renameError && (
                            <p role="alert" className="mt-0.5 px-1 text-xs text-destructive">
                              {renameError}
                            </p>
                          )}
                        </div>
                      )
                    })}
                    {group.works.length === 0 && (
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

      {/* 删除确认：会话 / 项目共用（物理删除不可恢复；非空项目级联删除会话） */}
      <Dialog
        open={confirm !== null}
        onOpenChange={(open) => {
          if (!open) closeConfirm()
        }}
      >
        <DialogContent className="sm:max-w-md" showCloseButton={false}>
          <DialogHeader>
            <DialogTitle className="text-base">
              {confirm?.kind === 'project' ? '删除项目' : '删除会话'}
            </DialogTitle>
            <DialogDescription className="break-words">
              {confirm?.kind === 'project' ? (
                confirm.sessionCount > 0 ? (
                  <>
                    项目 {confirm.name}（{confirm.path}）含 {confirm.sessionCount}{' '}
                    个会话，将一并永久删除，不可恢复。
                  </>
                ) : (
                  <>
                    项目 {confirm.name}（{confirm.path}）将从列表移除，不影响磁盘上的目录。
                  </>
                )
              ) : (
                <>会话「{confirm?.title}」将被永久删除（含名下全部 session 与消息记录），不可恢复。</>
              )}
            </DialogDescription>
          </DialogHeader>
          {confirmError && (
            <p role="alert" className="text-xs text-destructive">
              {confirmError}
            </p>
          )}
          <DialogFooter>
            <Button variant="outline" size="sm" disabled={confirming} onClick={closeConfirm}>
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              disabled={confirming}
              onClick={() => void submitConfirm()}
            >
              {confirming ? '删除中…' : '删除'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
