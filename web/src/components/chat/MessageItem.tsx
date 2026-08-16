// 单条消息渲染：user（右侧气泡）/ assistant（markdown + thinking 折叠 +
// 工具调用块）/ tool（执行卡片）/ system（居中提示）。

import { useState } from 'react'
import { AlertTriangle, ChevronRight, Cpu, Loader2 } from 'lucide-react'

import { Markdown } from '@/components/Markdown'
import { ToolCard } from '@/components/chat/ToolCard'
import { briefArgs } from '@/lib/toolArgs'
import { cn } from '@/lib/utils'
import type { AssistantItem, ChatItem, ToolItem, UserItem } from '@/lib/chat'

function UserMessage({ item }: { item: UserItem }) {
  return (
    <div className="flex justify-end">
      <div className="max-w-[85%] rounded-2xl rounded-br-sm bg-primary px-4 py-2.5 text-sm text-primary-foreground">
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
    </div>
  )
}

function ThinkingBlock({ thinking, streaming }: { thinking: string; streaming: boolean }) {
  const [open, setOpen] = useState(false)
  if (!thinking) return null
  return (
    <div className="mb-2 overflow-hidden rounded-lg border border-dashed">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-1.5 px-3 py-1.5 text-xs text-muted-foreground hover:bg-accent/50"
      >
        <ChevronRight className={cn('size-3.5 transition-transform', open && 'rotate-90')} />
        {streaming && <Loader2 className="size-3 animate-spin" />}
        <span className="font-medium">思考</span>
        <span className="flex-1 truncate text-[11px] italic">{thinking.split('\n')[0]}</span>
      </button>
      {open && (
        <div className="border-t bg-muted/30 px-3 py-2 text-xs italic leading-relaxed text-muted-foreground">
          <div className="whitespace-pre-wrap">{thinking}</div>
        </div>
      )}
    </div>
  )
}

function ToolCallChips({ blocks }: { blocks: AssistantItem['blocks'] }) {
  const calls = blocks.filter((block) => block.type === 'tool_call')
  if (calls.length === 0) return null
  return (
    <div className="mb-2 flex flex-wrap gap-1.5">
      {calls.map((call) => (
        <span
          key={call.id}
          className="inline-flex items-center gap-1 rounded-md bg-muted px-2 py-0.5 font-mono text-[11px] text-muted-foreground"
        >
          <Cpu className="size-3" />
          {call.name}
          {call.arguments && <span className="opacity-70">({briefArgs(call.name ?? '', call.arguments ?? {})})</span>}
        </span>
      ))}
    </div>
  )
}

function AssistantMessage({ item }: { item: AssistantItem }) {
  const failed = item.stopReason === 'error' || item.stopReason === 'aborted'
  return (
    <div className="flex justify-start">
      <div className="max-w-[90%] rounded-2xl rounded-bl-sm border bg-card px-4 py-3">
        <ThinkingBlock thinking={item.thinking} streaming={item.streaming} />
        <ToolCallChips blocks={item.blocks} />
        {failed ? (
          <div className="flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/5 p-3 text-sm">
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
          <Markdown>{item.text}</Markdown>
        ) : item.streaming ? (
          <div className="flex items-center gap-2 py-1 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" />
            思考中…
          </div>
        ) : null}
        {item.model && !item.streaming && (
          <div className="mt-2 text-right text-[10px] text-muted-foreground/70">
            {item.model}
            {item.usage && item.usage.total_tokens > 0
              ? ` · ${item.usage.total_tokens.toLocaleString()} tokens`
              : ''}
          </div>
        )}
      </div>
    </div>
  )
}

function SystemMessage({ text }: { text: string }) {
  return (
    <div className="flex justify-center">
      <span className="rounded-full bg-muted/60 px-3 py-1 text-[11px] text-muted-foreground">
        {text}
      </span>
    </div>
  )
}

export function MessageItem({ item }: { item: ChatItem }) {
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
