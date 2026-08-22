// 启动页工作区选择栏：在输入框上方选择新会话的归属工作区（无默认 workspace，
// 必须显式选择）。已登记 workspace 以下拉选择；「使用其他目录…」切换为内联
// 路径输入（回车确认，Esc 返回下拉；`~/` 展开由服务端处理，目录不存在时
// 服务端报错并就地展示在错误条）。

import { useState } from 'react'
import { Check, ChevronsUpDown, FolderOpen, FolderPlus } from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { workspaceName } from '@/lib/sessions'
import type { WorkspaceSummary } from '@/lib/types'

interface WorkspaceBarProps {
  /** 已登记的全部 workspace（可为空，此时只能手动输入目录） */
  workspaces: WorkspaceSummary[]
  /** 当前选中的工作区路径（'' 表示未选择） */
  value: string
  onChange: (path: string) => void
}

export function WorkspaceBar({ workspaces, value, onChange }: WorkspaceBarProps) {
  // 内联路径输入模式（下拉中「使用其他目录…」进入；Esc/回车退出）
  const [editing, setEditing] = useState(false)
  const [path, setPath] = useState('')

  const confirmPath = () => {
    const trimmed = path.trim()
    if (trimmed) onChange(trimmed)
    setPath('')
    setEditing(false)
  }

  return (
    <div className="mx-auto w-full max-w-page px-4 pt-2 sm:px-7">
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span className="shrink-0">工作区</span>
        {editing ? (
          <input
            type="text"
            value={path}
            autoFocus
            placeholder="目录路径，如 ~/code/proj"
            aria-label="工作区路径"
            className="h-7 min-w-0 flex-1 rounded-md border border-input bg-background px-2 text-xs outline-none transition-[color,box-shadow] placeholder:text-muted-foreground/60 focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/50"
            onChange={(e) => setPath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') confirmPath()
              else if (e.key === 'Escape') {
                setPath('')
                setEditing(false)
              }
            }}
          />
        ) : (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="sm"
                title={value || '选择工作区'}
                className="h-7 min-w-0 max-w-full justify-start gap-1.5 rounded-full border border-border bg-background px-3 text-xs font-normal hover:bg-accent"
              >
                <FolderOpen className="size-3 shrink-0 opacity-70" />
                <span className="min-w-0 truncate text-left">
                  {value ? workspaceName(value) : '选择工作区'}
                </span>
                <ChevronsUpDown className="size-3 shrink-0 opacity-60" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="max-h-96 w-80 max-w-[calc(100vw-2rem)] overflow-y-auto">
              {workspaces.map((ws) => (
                <DropdownMenuItem
                  key={ws.id}
                  onSelect={() => onChange(ws.path)}
                  className="flex items-center justify-between gap-2"
                >
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate">{workspaceName(ws.path)}</span>
                    <span className="truncate text-[10px] text-muted-foreground/70">
                      {ws.path}
                    </span>
                  </span>
                  {ws.path === value && <Check className="size-3.5 shrink-0 text-foreground" />}
                </DropdownMenuItem>
              ))}
              {workspaces.length > 0 && <DropdownMenuSeparator />}
              <DropdownMenuItem
                onSelect={() => {
                  setPath('')
                  setEditing(true)
                }}
                className="gap-2"
              >
                <FolderPlus className="size-3.5" />
                使用其他目录…
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        )}
        {/* 选中路径不在登记列表（手动输入的目录）时展示完整路径 */}
        {!editing && value && !workspaces.some((ws) => ws.path === value) && (
          <span className="min-w-0 truncate text-muted-foreground/70" title={value}>
            {value}
          </span>
        )}
      </div>
    </div>
  )
}
