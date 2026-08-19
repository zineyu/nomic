// 聊天主视图：顶栏（标题）+ 消息流 + 输入区 + 提问弹层 + 错误提示。页面列 max-w-page 居中。

import { useEffect, useState } from 'react'
import { AlertTriangle, Loader2, MessageCircleQuestion, PanelLeft, X } from 'lucide-react'

import { ChatInput } from '@/components/chat/ChatInput'
import { MessageList } from '@/components/chat/MessageList'
import { QuestionModal } from '@/components/chat/QuestionModal'
import { Button } from '@/components/ui/button'
import type { UseChat } from '@/hooks/useChat'

interface ChatViewProps extends UseChat {
  sidebarOpen: boolean
  onToggleSidebar: () => void
}

export function ChatView({
  sidebarOpen,
  onToggleSidebar,
  ...chat
}: ChatViewProps) {
  const {
    items,
    running,
    queued,
    question,
    error,
    session,
    model,
    reasoning,
    contextTokens,
    send,
    stop,
    answerQuestion,
    dismissError,
    refreshSnapshot,
  } = chat

  const [minimizedId, setMinimizedId] = useState<string | null>(null)

  // 运行中按 Escape 停止当前运行
  useEffect(() => {
    if (!running) return
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') void stop()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [running, stop])

  const title = session?.title ?? 'nomic'
  const modelSpec = model ? `${model.provider}/${model.id}` : null
  const isMinimized = question && minimizedId === question.id

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col bg-background">
      {/* 顶栏：标题 */}
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-2.5">
        <Button
          variant="ghost"
          size="icon"
          className="size-8 shrink-0 md:hidden"
          onClick={onToggleSidebar}
          title={sidebarOpen ? '收起侧栏' : '展开侧栏'}
        >
          <PanelLeft className="size-4" />
        </Button>

        <div className="flex items-center gap-2">
          {/* 标题 */}
          <h2 className="truncate text-base font-semibold" title={title}>
            {title}
          </h2>
          {running && (
            <Loader2 className="size-3.5 shrink-0 animate-spin text-muted-foreground" />
          )}
        </div>

        <div className="flex-1" />
      </div>

      <div className="min-h-0 flex-1">
        <MessageList items={items} onExample={send} />
      </div>

      {error && (
        <div
          role="alert"
          className="mx-auto mb-2 flex w-full max-w-page items-center gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-destructive"
        >
          <AlertTriangle className="size-4 shrink-0" />
          <span className="min-w-0 flex-1 break-words">{error}</span>
          <button
            type="button"
            onClick={dismissError}
            className="shrink-0 rounded p-0.5 hover:bg-destructive/10"
            aria-label="关闭错误提示"
          >
            <X className="size-3.5" />
          </button>
        </div>
      )}

      <ChatInput
        running={running}
        queued={queued}
        modelSpec={modelSpec}
        reasoning={reasoning}
        contextTokens={contextTokens}
        contextWindow={model?.context_window ?? null}
        onSend={send}
        onStop={stop}
        onModelChanged={() => void refreshSnapshot()}
      />

      {question && !isMinimized && (
        <QuestionModal
          key={question.id}
          id={question.id}
          question={question.question}
          onAnswer={answerQuestion}
          onMinimize={() => setMinimizedId(question.id)}
        />
      )}

      {isMinimized && (
        <button
          type="button"
          onClick={() => setMinimizedId(null)}
          className="fixed right-4 bottom-12 z-50 flex items-center gap-2 rounded-full border bg-background px-4 py-2 text-xs font-medium shadow-lg transition-colors hover:bg-accent"
        >
          <MessageCircleQuestion className="size-4 text-primary" />
          待回答问题
        </button>
      )}
    </div>
  )
}
