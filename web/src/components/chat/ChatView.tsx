// 聊天主视图：顶栏（标题）+ 消息流 + 输入区 + 提问弹层 + 错误提示。页面列 max-w-page 居中。
// 启动页（未选中任何 session）：输入框上方渲染工作区选择栏（WorkspaceBar），
// 首条消息在选定 workspace 下创建 session；无默认 workspace。

import { useCallback, useEffect, useRef, useState } from 'react'
import { AlertTriangle, MessageCircleQuestion, PanelLeft, X } from 'lucide-react'

import { ChatInput } from '@/components/chat/ChatInput'
import { MessageList } from '@/components/chat/MessageList'
import { QuestionModal } from '@/components/chat/QuestionModal'
import { RunHint } from '@/components/chat/RunHint'
import { WorkspaceBar } from '@/components/chat/WorkspaceBar'
import { Button } from '@/components/ui/button'
import { fadeSlideIn } from '@/lib/anim'
import { runPhase } from '@/lib/chat'
import type { UseChat } from '@/hooks/useChat'

interface ChatViewProps extends UseChat {
  sidebarOpen: boolean
  onToggleSidebar: () => void
}

/** 错误提示横幅：出现时自下方滑入。 */
function ErrorBanner({ error, onDismiss }: { error: string; onDismiss: () => void }) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (ref.current) fadeSlideIn(ref.current, { y: 8, duration: 0.25 })
  }, [])
  return (
    <div
      ref={ref}
      role="alert"
      className="mx-auto mb-2 flex w-full max-w-page items-center gap-2 rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-xs text-destructive"
    >
      <AlertTriangle className="size-4 shrink-0" />
      <span className="min-w-0 flex-1 break-words">{error}</span>
      <button
        type="button"
        onClick={onDismiss}
        className="shrink-0 rounded p-0.5 hover:bg-destructive/10"
        aria-label="关闭错误提示"
      >
        <X className="size-3.5" />
      </button>
    </div>
  )
}

/** 最小化的待回答问题入口：弹出式入场。 */
function MinimizedQuestionButton({ onClick }: { onClick: () => void }) {
  const ref = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    if (ref.current) fadeSlideIn(ref.current, { y: 8, duration: 0.25 })
  }, [])
  return (
    <button
      ref={ref}
      type="button"
      onClick={onClick}
      className="fixed right-4 bottom-12 z-50 flex items-center gap-2 rounded-full border bg-background px-4 py-2 text-xs font-medium shadow-lg transition-colors hover:bg-accent"
    >
      <MessageCircleQuestion className="size-4 text-primary" />
      待回答问题
    </button>
  )
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
    sessionId,
    model,
    reasoning,
    contextTokens,
    workspaces,
    send,
    stop,
    startSession,
    answerQuestion,
    dismissError,
    switchModel,
  } = chat

  const [minimizedId, setMinimizedId] = useState<string | null>(null)

  // 启动页（无默认 workspace/session）：未选中任何 session 时展示工作区选择栏
  const startPage = sessionId === null
  // 用户手动选择/输入的目录；未选择时回落到最近活跃的 workspace（仅 UI 预选）
  const [startChoice, setStartChoice] = useState<string | null>(null)
  const startWorkspace = startChoice ?? workspaces[0]?.path ?? ''

  // 启动页发送：在选定 workspace 下创建 session 并发出首条消息
  const handleStartSend = useCallback(
    (text: string) => {
      if (!startWorkspace) return
      void startSession(startWorkspace, text)
    },
    [startSession, startWorkspace],
  )

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
        </div>

        <div className="flex-1" />
      </div>

      <div className="min-h-0 flex-1">
        <MessageList
          items={items}
          onExample={startPage ? (startWorkspace ? handleStartSend : undefined) : send}
        />
      </div>

      {error && <ErrorBanner error={error} onDismiss={dismissError} />}

      {startPage && (
        <WorkspaceBar
          workspaces={workspaces}
          value={startWorkspace}
          onChange={setStartChoice}
        />
      )}

      {/* 运行状态提示（输入框上方；空闲时不渲染） */}
      <RunHint phase={runPhase(items, running)} />

      <ChatInput
        running={running}
        queued={queued}
        modelSpec={modelSpec}
        reasoning={reasoning}
        contextTokens={contextTokens}
        contextWindow={model?.context_window ?? null}
        sessionId={sessionId}
        sendDisabled={startPage && !startWorkspace}
        placeholder={
          startPage && !startWorkspace ? '先选择工作区，再给智能体发消息' : undefined
        }
        onSend={startPage ? handleStartSend : send}
        onStop={stop}
        onSwitchModel={switchModel}
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
        <MinimizedQuestionButton onClick={() => setMinimizedId(null)} />
      )}
    </div>
  )
}
