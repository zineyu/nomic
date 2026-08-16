// 应用布局：侧栏（会话/模型）+ 聊天主区 + 状态栏 + 提问弹层。

import { ChatView } from '@/components/chat/ChatView'
import { Sidebar } from '@/components/Sidebar'
import { StatusBar } from '@/components/StatusBar'
import { TooltipProvider } from '@/components/ui/tooltip'
import { useChat } from '@/hooks/useChat'

export default function App() {
  const chat = useChat()
  const modelSpec = chat.model ? `${chat.model.provider}/${chat.model.id}` : null

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-dvh flex-col bg-background text-foreground">
        <div className="flex min-h-0 flex-1">
          <Sidebar
            currentSessionId={chat.session?.id ?? null}
            modelSpec={modelSpec}
            reasoning={chat.reasoning}
            cwd={chat.cwd}
            onNewSession={() => void chat.newSession()}
            onResume={(id) => void chat.resumeSession(id)}
            onModelChanged={() => void chat.refreshSnapshot()}
          />
          <ChatView {...chat} />
        </div>
        <StatusBar
          model={modelSpec}
          contextTokens={chat.contextTokens}
          running={chat.running}
          queued={chat.queued}
          sessionTitle={chat.session?.title ?? null}
        />
      </div>
    </TooltipProvider>
  )
}
