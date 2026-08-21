// 图标导航栏：模仿 DeepSeek Harness 左侧导航。
// 顶部 logo + 会话入口。

import { MessagesSquare } from 'lucide-react'

export function Rail() {
  return (
    <nav className="hidden w-14 shrink-0 flex-col items-center border-r bg-sidebar py-4 md:flex">
      <div className="mb-5 flex items-center gap-1.5">
        <img src="/favicon.svg" alt="nomic" className="size-8 rounded-lg" />
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
