// 应用布局：顶栏（面包屑/主题）+ 图标导航栏 + 侧栏（会话）+
// 聊天主区。参考 Kimi 布局：上下文用量并入侧栏底部，不再有独立状态栏。

import { useCallback, useEffect, useState } from 'react'

import { ChatView } from '@/components/chat/ChatView'
import { Rail } from '@/components/Rail'
import { Sidebar } from '@/components/Sidebar'
import { Topbar } from '@/components/Topbar'
import { TooltipProvider } from '@/components/ui/tooltip'
import { useChat } from '@/hooks/useChat'

const MD_QUERY = '(min-width: 768px)'

export default function App() {
  const chat = useChat()
  const [sidebarOpen, setSidebarOpen] = useState(() =>
    window.matchMedia(MD_QUERY).matches,
  )

  // 窄屏选中会话后自动收起侧栏
  const handleResume = useCallback(
    (id: string) => {
      void chat.resumeSession(id)
      if (!window.matchMedia(MD_QUERY).matches) setSidebarOpen(false)
    },
    [chat],
  )

  // 监听断点变化：进入桌面端自动展开
  useEffect(() => {
    const mql = window.matchMedia(MD_QUERY)
    const onChange = (e: MediaQueryListEvent) => {
      if (e.matches) setSidebarOpen(true)
    }
    mql.addEventListener('change', onChange)
    return () => mql.removeEventListener('change', onChange)
  }, [])

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-dvh flex-col bg-background text-foreground">
        <Topbar />
        <div className="flex min-h-0 flex-1">
          {/* 图标导航栏（桌面端） */}
          <Rail />
          {/* 移动端遮罩 */}
          {sidebarOpen && (
            <div
              className="fixed inset-0 z-40 bg-black/40 md:hidden"
              onClick={() => setSidebarOpen(false)}
            />
          )}
          {/* 侧栏：移动端 fixed overlay，桌面端 in-flow */}
          <div
            className={
              sidebarOpen
                ? 'fixed inset-y-0 left-0 z-50 w-64 md:static md:z-auto'
                : 'hidden'
            }
          >
            <Sidebar
              sessions={chat.sessions}
              currentSessionId={chat.session?.id ?? null}
              cwd={chat.cwd}
              contextTokens={chat.contextTokens}
              contextWindow={chat.model?.context_window ?? null}
              running={chat.running}
              onNewSession={() => void chat.newSession()}
              onResume={handleResume}
            />
          </div>
          <ChatView
            {...chat}
            sidebarOpen={sidebarOpen}
            onToggleSidebar={() => setSidebarOpen((v) => !v)}
          />
        </div>
      </div>
    </TooltipProvider>
  )
}
