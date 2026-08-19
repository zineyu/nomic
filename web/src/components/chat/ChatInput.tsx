// 输入区：模仿 DeepSeek Harness 布局。
// 输入框占满剩余空间，右下角模型选择器 + 发送按钮。

import { useEffect, useRef, useState } from 'react'
import { SendHorizontal, Square } from 'lucide-react'

import { ContextRing } from '@/components/chat/ContextRing'
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
  contextTokens?: number
  contextWindow?: number | null
  onModelChanged?: () => void
  onSend: (text: string) => void
  onStop: () => void
}

export function ChatInput({
  running,
  queued,
  modelSpec,
  reasoning,
  contextTokens = 0,
  contextWindow = null,
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
    <div className="mx-auto w-full max-w-[920px] px-7 pb-2 pt-1.5">
      {queued > 0 && (
        <div className="mb-2 text-center text-xs text-muted-foreground">
          已排队 {queued} 条，当前轮完成后按序发送
        </div>
      )}
      <div className={cn('rounded-xl border bg-card shadow-sm', 'focus-within:ring-1 focus-within:ring-ring/40')}>
        {/* 输入区主体 */}
        <div className="flex items-start gap-2 px-3.5 pt-2">
          {/* 输入框 */}
          <Textarea
            ref={textareaRef}
            value={value}
            onChange={(e) => setValue(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder="给智能体发消息"
            rows={1}
            autoFocus
            style={{ maxHeight: MAX_HEIGHT }}
            className="min-h-7 flex-1 resize-none border-0 bg-transparent px-0 py-0.5 text-base shadow-none focus-visible:ring-0 placeholder:text-muted-foreground/50"
          />
        </div>

        {/* 底部操作栏 */}
        <div className="flex items-center gap-2 px-3.5 pb-2 pt-1">
          <div className="flex-1" />

          {/* 模型选择器 */}
          {modelSpec && onModelChanged && (
            <ModelPicker
              currentSpec={modelSpec}
              reasoning={reasoning ?? null}
              onChanged={onModelChanged}
            />
          )}

          {/* 上下文用量环形指示器 */}
          <ContextRing tokens={contextTokens} window={contextWindow} />

          {/* 发送/停止按钮 */}
          {running ? (
            <Button
              type="button"
              size="icon"
              variant="outline"
              onClick={onStop}
              className="size-7 shrink-0 rounded-full"
              title="停止当前运行（队列保留）"
            >
              <Square className="size-3 fill-current" />
            </Button>
          ) : (
            <Button
              type="button"
              size="icon"
              onClick={submit}
              disabled={!value.trim()}
              className="size-7 shrink-0 rounded-full"
              title="发送（Enter）"
            >
              <SendHorizontal className="size-3.5" />
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}