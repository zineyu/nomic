// 输入区：多行输入 + 发送/停止；运行中发送排队（服务端队列，状态栏提示）。
// Enter 发送，Shift+Enter 换行。

import { useEffect, useRef, useState } from 'react'
import { SendHorizontal, Square } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'

interface ChatInputProps {
  running: boolean
  queued: number
  onSend: (text: string) => void
  onStop: () => void
}

export function ChatInput({ running, queued, onSend, onStop }: ChatInputProps) {
  const [value, setValue] = useState('')
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  // 输入自动增高（上限 8 行）
  useEffect(() => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, 8 * 24)}px`
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
    <div className="border-t bg-background/95 p-3 backdrop-blur">
      <div className="mx-auto max-w-3xl">
        {queued > 0 && (
          <div className="mb-2 text-center text-[11px] text-muted-foreground">
            已排队 {queued} 条，当前轮完成后按序发送
          </div>
        )}
        <div className={cn('flex items-end gap-2 rounded-xl border bg-card p-2 shadow-sm')}>
          <Textarea
            ref={textareaRef}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder="输入消息，Enter 发送（Shift+Enter 换行）"
            rows={1}
            className="max-h-52 min-h-9 flex-1 resize-none border-0 bg-transparent p-1.5 shadow-none focus-visible:ring-0"
          />
          {running ? (
            <Button
              type="button"
              size="icon"
              variant="outline"
              onClick={onStop}
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
              title="发送（Enter）"
            >
              <SendHorizontal className="size-4" />
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}
