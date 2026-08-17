// 图标导航栏：logo + 会话入口（当前唯一视图）。桌面端固定展示。

import { MessagesSquare } from 'lucide-react'

export function Rail() {
  return (
    <nav className="hidden w-14 shrink-0 flex-col items-center border-r bg-sidebar py-4 md:flex">
      <div className="mb-5 flex size-8 items-center justify-center rounded-lg bg-primary text-sm font-bold text-primary-foreground">
        n
      </div>
      <button
        type="button"
        className="flex size-10 items-center justify-center rounded-lg bg-sidebar-accent text-sidebar-ring"
        title="会话"
        aria-label="会话"
      >
        <MessagesSquare className="size-[18px]" />
      </button>
    </nav>
  )
}
