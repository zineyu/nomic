// 输入区：多行输入 + 发送/停止；运行中发送排队（服务端队列，状态栏提示）。
// Enter 发送，Shift+Enter 换行。

import { useEffect, useRef, useState } from 'react'
import { SendHorizontal, Square } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'

const MAX_LINES = 8
const LINE_HEIGHT = 24
const MAX_HEIGHT = MAX_LINES * LINE_HEIGHT

interface ChatInputProps {
  running: boolean
  queued: number
  onSend: (text: string) => void
  onStop: () => void
}

export function ChatInput({ running, queued, onSend, onStop }: ChatInputProps) {
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
    <div className="border-t bg-background p-4">
      <div className="mx-auto max-w-3xl">
        {queued > 0 && (
          <div className="mb-2 text-center text-xs text-muted-foreground">
            已排队 {queued} 条，当前轮完成后按序发送
          </div>
        )}
        <div
          className={cn(
            'flex items-end gap-2 rounded-2xl border bg-card p-2 shadow-sm',
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
            className="min-h-10 flex-1 resize-none border-0 bg-transparent px-3 py-2.5 shadow-none focus-visible:ring-0"
          />
          {running ? (
            <Button
              type="button"
              size="icon"
              variant="outline"
              onClick={onStop}
              className="size-9 rounded-xl"
              title="停止当前运行（队列保留）"
            >
              <Square className="size-4 fill-current" />
            </Button>
          ) : (
            <Button
              type="button"
              size="icon"
              onClick={submit}
              disabled={!value.trim()}
              className="size-9 rounded-xl"
              title="发送（Enter）"
            >
              <SendHorizontal className="size-4" />
            </Button>
          )}
        </div>
        <div className="mt-1.5 text-center text-xs text-muted-foreground">
          Enter 发送 · Shift+Enter 换行
        </div>
      </div>
    </div>
  )
}
