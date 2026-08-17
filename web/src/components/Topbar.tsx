// 顶栏：macOS 风格红绿灯（装饰）+ 居中面包屑 + 主题切换。
// 布局参考 Kimi 风格：面包屑居中、主题按钮靠右，红绿灯仅桌面端可见。

import { Moon, Monitor, Sun } from 'lucide-react'

import { useTheme } from '@/hooks/useTheme'

export function Topbar() {
  const { theme, cycle } = useTheme()
  const ThemeIcon = theme === 'dark' ? Moon : theme === 'light' ? Sun : Monitor

  return (
    <header className="relative flex h-12 shrink-0 items-center gap-2.5 border-b bg-background px-4">
      <div className="hidden gap-2 md:flex" aria-hidden="true">
        <span className="size-2.5 rounded-full bg-[#FF5F57]" />
        <span className="size-2.5 rounded-full bg-[#FEBC2E]" />
        <span className="size-2.5 rounded-full bg-[#28C840]" />
      </div>
      <div className="absolute left-1/2 -translate-x-1/2 text-xs text-muted-foreground">
        nomic / <span className="font-medium text-foreground">web</span>
      </div>
      <button
        type="button"
        onClick={cycle}
        className="ml-auto flex size-7 items-center justify-center rounded-md border border-border bg-background text-muted-foreground transition-colors hover:text-foreground"
        title={`主题：${theme === 'light' ? '浅色' : theme === 'dark' ? '深色' : '跟随系统'}`}
        aria-label="切换主题"
      >
        <ThemeIcon className="size-3.5" />
      </button>
    </header>
  )
}
