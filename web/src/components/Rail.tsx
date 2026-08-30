// 图标导航栏：模仿 DeepSeek Harness 左侧导航。
// 顶部 logo + 会话 / 设置入口。

import { MessagesSquare, Settings } from 'lucide-react'

import { cn } from '@/lib/utils'

export type AppView = 'chat' | 'settings'

interface RailProps {
  view: AppView
  onNavigate: (view: AppView) => void
}

export function Rail({ view, onNavigate }: RailProps) {
  const item = (
    target: AppView,
    label: string,
    Icon: typeof MessagesSquare,
  ) => (
    <button
      type="button"
      onClick={() => onNavigate(target)}
      aria-current={view === target ? 'page' : undefined}
      className={cn(
        'flex size-10 items-center justify-center rounded-lg transition-all outline-none focus-visible:ring-2 focus-visible:ring-ring/50 active:scale-95',
        view === target
          ? 'bg-foreground text-background hover:bg-foreground/85'
          : 'text-muted-foreground hover:bg-sidebar-accent hover:text-foreground',
      )}
      title={label}
      aria-label={label}
    >
      <Icon className="size-[18px]" />
    </button>
  )

  return (
    <nav className="hidden w-14 shrink-0 flex-col items-center border-r bg-sidebar py-4 md:flex">
      <div className="mb-5 flex items-center gap-1.5">
        <img src="/favicon.svg" alt="nomic" className="size-8 rounded-lg" />
      </div>
      <div className="flex flex-col gap-2">
        {item('chat', '会话', MessagesSquare)}
        {item('settings', '设置', Settings)}
      </div>
    </nav>
  )
}
