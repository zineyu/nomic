// 聊天主视图：消息列表 + 输入区 + 提问弹层 + 错误提示。

import { AlertTriangle, X } from 'lucide-react'

import { ChatInput } from '@/components/chat/ChatInput'
import { MessageList } from '@/components/chat/MessageList'
import { QuestionModal } from '@/components/chat/QuestionModal'
import type { UseChat } from '@/hooks/useChat'

export function ChatView(chat: UseChat) {
  const {
    items,
    running,
    queued,
    question,
    error,
    send,
    stop,
    answerQuestion,
    dismissError,
  } = chat

  return (
    <div className="flex h-full min-w-0 flex-1 flex-col">
      <div className="min-h-0 flex-1">
        <MessageList items={items} />
      </div>

      {error && (
        <div className="mx-auto mb-2 flex w-full max-w-3xl items-center gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 px-4 text-xs text-destructive">
          <AlertTriangle className="size-4 shrink-0" />
          <span className="min-w-0 flex-1 truncate">{error}</span>
          <button
            type="button"
            onClick={dismissError}
            className="shrink-0 rounded p-0.5 hover:bg-destructive/10"
          >
            <X className="size-3.5" />
          </button>
        </div>
      )}

      <ChatInput running={running} queued={queued} onSend={send} onStop={stop} />

      {question && (
        <QuestionModal
          key={question.id}
          id={question.id}
          question={question.question}
          onAnswer={answerQuestion}
        />
      )}
    </div>
  )
}
