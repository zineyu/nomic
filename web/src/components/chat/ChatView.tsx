// 聊天主视图：会话头（标题 + 模型元信息 + 分隔线）+ 消息流 + 输入区 +
// 提问弹层 + 错误提示。列宽 920px 居中，与参考设计一致。

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
      {/* 会话头：标题 + 模型/上下文元信息 */}
      <div className="mx-auto w-full max-w-[920px] px-7 pt-3">
        <div className="flex items-center gap-2">
          <Button
            variant="ghost"
            size="icon"
            className="size-8 shrink-0 md:hidden"
            onClick={onToggleSidebar}
            title={sidebarOpen ? '收起侧栏' : '展开侧栏'}
          >
            <PanelLeft className="size-4" />
          </Button>
          <h2 className="min-w-0 flex-1 truncate text-sm font-semibold" title={title}>
            {title}
          </h2>
          {running && (
            <span className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 className="size-3 animate-spin" />
              运行中
            </span>
          )}
        </div>
        <p className="mt-1 text-xs text-muted-foreground">
          {modelSpec ?? '未选择模型'}
          {reasoning ? ` · ${reasoning}` : ''}
          {contextTokens > 0 ? ` · 上下文 ${contextTokens.toLocaleString()} tokens` : ''}
        </p>
        <hr className="mt-3 border-border" />
      </div>

      <div className="min-h-0 flex-1">
        <MessageList items={items} onExample={send} />
      </div>

      {error && (
        <div
          role="alert"
          className="mx-auto mb-2 flex w-full max-w-[920px] items-center gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-destructive"
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
