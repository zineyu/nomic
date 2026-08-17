// 输入区：输入卡片（多行输入 + 模型胶囊 + 圆形发送/停止）+ 快捷键提示。
// Enter 发送，Shift+Enter 换行；运行中发送排队（服务端队列，提示排队数）。

import { useEffect, useRef, useState } from 'react'
import { SendHorizontal, Square } from 'lucide-react'

import { ModelPicker } from '@/components/ModelPicker'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'

const MAX_LINES = 8
const LINE_HEIGHT = 24
const MAX_HEIGHT = MAX_LINES * LINE_HEIGHT

interface ChatInputProps {
  running: boolean
  queued: number
  modelSpec?: string | null
  reasoning?: string | null
  onModelChanged?: () => void
  onSend: (text: string) => void
  onStop: () => void
}

export function ChatInput({
  running,
  queued,
  modelSpec,
  reasoning,
  onModelChanged,
  onSend,
  onStop,
}: ChatInputProps) {
  const [value, setValue] = useState('')
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  // 输入自动增高（上限 MAX_LINES 行）
  useEffect(() => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, MAX_HEIGHT)}px`
  }, [value])

  const submit = () => {
    const text = value.trim()
    if (!text) return
    onSend(text)
    setValue('')
  }

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault()
      submit()
    }
  }

  return (
    <div className="mx-auto w-full max-w-[920px] px-7 pb-2 pt-2">
      {queued > 0 && (
        <div className="mb-2 text-center text-xs text-muted-foreground">
          已排队 {queued} 条，当前轮完成后按序发送
        </div>
      )}
      <div
        className={cn(
          'rounded-[14px] border bg-card p-3.5 pb-2.5 shadow-sm',
          'focus-within:ring-1 focus-within:ring-ring/40',
        )}
      >
        <Textarea
          ref={textareaRef}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="给 nomic 发消息…"
          rows={1}
          autoFocus
          style={{ maxHeight: MAX_HEIGHT }}
          className="min-h-10 flex-1 resize-none border-0 bg-transparent px-1 py-1 text-sm shadow-none focus-visible:ring-0"
        />
        <div className="mt-2 flex items-center gap-2.5">
          {modelSpec && onModelChanged && (
            <ModelPicker
              currentSpec={modelSpec}
              reasoning={reasoning ?? null}
              onChanged={onModelChanged}
            />
          )}
          {running ? (
            <Button
              type="button"
              size="icon"
              variant="outline"
              onClick={onStop}
              className="ml-auto size-8 shrink-0 rounded-full"
              title="停止当前运行（队列保留）"
            >
              <Square className="size-3.5 fill-current" />
            </Button>
          ) : (
            <Button
              type="button"
              size="icon"
              onClick={submit}
              disabled={!value.trim()}
              className="ml-auto size-8 shrink-0 rounded-full"
              title="发送（Enter）"
            >
              <SendHorizontal className="size-3.5" />
            </Button>
          )}
        </div>
      </div>
      <div className="mt-2 pb-1 text-center text-xs text-muted-foreground">
        Enter 发送 · Shift+Enter 换行
      </div>
    </div>
  )
}
