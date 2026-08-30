// 应用布局：顶栏（面包屑/主题）+ 图标导航栏 + 侧栏（会话）+
// 聊天主区。参考 Kimi 布局：上下文用量并入侧栏底部，不再有独立状态栏。

import { useCallback, useEffect, useState } from 'react'

import { ChatView } from '@/components/chat/ChatView'
import { Rail, type AppView } from '@/components/Rail'
import { SettingsView } from '@/components/settings/SettingsView'
import { Sidebar } from '@/components/Sidebar'
import { StatusBar } from '@/components/StatusBar'
import { TooltipProvider } from '@/components/ui/tooltip'
import { useChat } from '@/hooks/useChat'

const MD_QUERY = '(min-width: 768px)'

export default function App() {
  const chat = useChat()
  const [view, setView] = useState<AppView>('chat')
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
        <div className="flex min-h-0 flex-1">
          {/* 图标导航栏（桌面端） */}
          <Rail view={view} onNavigate={setView} />
          {/* 移动端遮罩 */}
          {view === 'chat' && sidebarOpen && (
            <div
              className="fixed inset-0 z-40 bg-black/40 md:hidden"
              onClick={() => setSidebarOpen(false)}
            />
          )}
          {/* 侧栏：仅会话视图；移动端 fixed overlay，桌面端 in-flow */}
          {view === 'chat' && (
            <div
              className={
                sidebarOpen
                  ? 'fixed inset-y-0 left-0 z-50 w-80 max-w-[85vw] md:static md:z-auto'
                  : 'hidden'
              }
            >
              <Sidebar
                sessions={chat.sessions}
                workspaces={chat.workspaces}
                currentSessionId={chat.sessionId}
                running={chat.running}
                onNewSession={(ws) => void chat.newSession(ws)}
                onAddWorkspace={chat.addWorkspace}
                onRenameSession={chat.renameSession}
                onDeleteSession={chat.deleteSession}
                onDeleteWorkspace={chat.deleteWorkspace}
                onResume={handleResume}
              />
            </div>
          )}
          <div className="flex min-h-0 min-w-0 flex-1 flex-col">
            {view === 'chat' ? (
              <>
                <ChatView
                  {...chat}
                  sidebarOpen={sidebarOpen}
                  onToggleSidebar={() => setSidebarOpen((v) => !v)}
                />
                <StatusBar stats={chat.stats} running={chat.running} />
              </>
            ) : (
              <SettingsView />
            )}
          </div>
        </div>
      </div>
    </TooltipProvider>
  )
}
