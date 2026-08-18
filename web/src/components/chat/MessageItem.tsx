// 单条消息渲染：user（右侧气泡 + 时间元信息）/ assistant（回合头部 nomic
// 头像 + 思考胶囊 + markdown 正文 + 流式光标）/ tool（执行卡片，独立渲染）/
// system（居中提示）。参考 Kimi 布局：助手回合左对齐、正文优先。

import { memo, useCallback, useState } from 'react'
import { AlertTriangle, Brain, Check, ChevronDown, Copy, Loader2 } from 'lucide-react'

import { Markdown } from '@/components/Markdown'
import { useThrottledValue } from '@/hooks/useThrottledValue'
import { cn } from '@/lib/utils'
import type { AssistantItem, ChatItem, ToolItem, UserItem } from '@/lib/chat'
import { ToolCard } from './ToolCard'

function formatMessageTime(millis: number): string {
  const date = new Date(millis)
  const now = new Date()
  const sameDay = date.toDateString() === now.toDateString()
  const time = date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' })
  if (sameDay) return time
  return `${date.toLocaleDateString('zh-CN', { month: '2-digit', day: '2-digit' })} ${time}`
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  const onCopy = useCallback(() => {
    void navigator.clipboard.writeText(text).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }, [text])

  return (
    <button
      type="button"
      onClick={onCopy}
      className="flex size-6 items-center justify-center rounded text-muted-foreground transition-colors hover:text-foreground"
      aria-label={copied ? '已复制' : '复制消息'}
      title={copied ? '已复制' : '复制消息'}
    >
      {copied ? <Check className="size-3 text-success" /> : <Copy className="size-3" />}
    </button>
  )
}

function UserMessage({ item }: { item: UserItem }) {
  return (
    <div className="flex flex-col items-end gap-1">
      <div className="max-w-[70%] rounded-[14px] rounded-br-md bg-primary px-4 py-2.5 text-base leading-relaxed text-primary-foreground shadow-sm">
        {item.images.length > 0 && (
          <div className="mb-2 flex flex-wrap gap-2">
            {item.images.map((image, i) => (
              <img
                key={i}
                src={`data:${image.mime_type};base64,${image.data}`}
                alt="附件"
                className="max-h-48 rounded-lg border border-primary-foreground/20"
              />
            ))}
          </div>
        )}
        <div className="whitespace-pre-wrap">{item.text}</div>
      </div>
      <div className="pr-1 text-[11px] text-muted-foreground">
        {item.timestamp ? `${formatMessageTime(item.timestamp)} · ` : ''}你
      </div>
    </div>
  )
}

function ThinkingPill({ thinking, streaming }: { thinking: string; streaming: boolean }) {
  const [expanded, setExpanded] = useState(false)
  if (!thinking) return null

  // 取第一行非空内容作为摘要
  const firstLine = thinking.split('\n').find((line) => line.trim() !== '') ?? thinking
  const brief = firstLine.length > 80 ? firstLine.slice(0, 80) + '…' : firstLine

  return (
    <div className="max-w-[920px] text-gray-500">
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        className="flex h-6 w-full items-center gap-1.5 px-3 text-left text-xs cursor-pointer hover:bg-accent/60"
      >
        <Brain className="size-3 shrink-0 text-gray-500" />
        <span className="shrink-0 font-semibold text-gray-500">Think</span>
        <span className="min-w-0 flex-1 truncate text-xs text-gray-500">
          {streaming ? '思考中…' : brief}
        </span>
        {streaming && <Loader2 className="size-3 shrink-0 animate-spin text-gray-500" />}
        <ChevronDown
          className={cn(
            'size-3 shrink-0 text-gray-500 transition-transform',
            expanded && 'rotate-180',
          )}
        />
      </button>
      {expanded && (
        <div className="px-3 py-1.5">
          <div className="max-h-48 overflow-auto whitespace-pre-wrap rounded bg-muted/30 p-2 text-xs leading-relaxed text-gray-500">
            {thinking}
          </div>
        </div>
      )}
    </div>
  )
}

function AssistantMessage({ item }: { item: AssistantItem }) {
  const failed = item.stopReason === 'error' || item.stopReason === 'aborted'
  // 流式期间节流文本，避免每个字符触发一次 markdown 解析；定稿后立即冲刷。
  const displayText = useThrottledValue(item.text, item.streaming ? 80 : 0)
  return (
    <div className="flex flex-col gap-1.5">

      <ThinkingPill thinking={item.thinking} streaming={item.streaming} />

      {failed ? (
        <div className="flex max-w-[920px] items-start gap-2 rounded-xl border border-destructive/40 bg-destructive/5 p-3 text-sm">
          <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
          <div>
            <div className="font-medium text-destructive">
              {item.stopReason === 'aborted' ? '已中止' : '响应失败'}
            </div>
            {item.errorMessage && (
              <div className="mt-1 text-xs text-muted-foreground">{item.errorMessage}</div>
            )}
          </div>
        </div>
      ) : item.text ? (
        <div className="max-w-[920px]">
          <Markdown>{displayText}</Markdown>
          {item.streaming && <span className="caret">▍</span>}
        </div>
      ) : item.streaming ? (
        <div className="flex items-center gap-2 py-1 text-xs text-muted-foreground">
          <Loader2 className="size-3 animate-spin" />
          正在生成回复…
        </div>
      ) : null}

      {item.text && !item.streaming && (
        <div className="flex max-w-[920px] justify-end">
          <CopyButton text={item.text} />
        </div>
      )}
    </div>
  )
}

function SystemMessage({ text }: { text: string }) {
  return (
    <div className="flex justify-center">
      <span className="rounded-full bg-muted/60 px-3 py-1 text-xs text-muted-foreground">
        {text}
      </span>
    </div>
  )
}

function MessageItemImpl({ item }: { item: ChatItem }) {
  switch (item.type) {
    case 'user':
      return <UserMessage item={item} />
    case 'assistant':
      return <AssistantMessage item={item} />
    case 'tool':
      return <ToolCard item={item as ToolItem} />
    case 'system':
      return <SystemMessage text={item.text} />
  }
}

// memo：事件增量合并时仅最后一个流式项引用变化，历史消息跳过重渲染。
export const MessageItem = memo(MessageItemImpl)
