// 消息列表：滚动容器 + 贴近底部时自动跟随新内容；上滚后显示「跳到最新」。

import { useEffect, useRef, useState } from 'react'
import { ArrowDown } from 'lucide-react'

import { MessageItem } from '@/components/chat/MessageItem'
import type { ChatItem } from '@/lib/chat'

const EXAMPLES = [
  '帮我看一下这个项目的整体结构',
  '用 Rust 写一个快速排序并解释复杂度',
  '给当前改动写一份 commit message',
]

export function MessageList({
  items,
  onExample,
}: {
  items: ChatItem[]
  onExample?: (text: string) => void
}) {
  const scrollRef = useRef<HTMLDivElement>(null)
  const stickToBottom = useRef(true)
  const [showJump, setShowJump] = useState(false)

  const scrollToBottom = () => {
    const el = scrollRef.current
    if (el) el.scrollTop = el.scrollHeight
    stickToBottom.current = true
    setShowJump(false)
  }

  // 用户主动上滚时停止跟随，回到底部恢复
  const onScroll = () => {
    const el = scrollRef.current
    if (!el) return
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    stickToBottom.current = atBottom
    setShowJump(!atBottom)
  }

  useEffect(() => {
    const el = scrollRef.current
    if (el && stickToBottom.current) {
      el.scrollTop = el.scrollHeight
    }
  }, [items])

  if (items.length === 0) {
    return (
      <div className="flex h-full items-center justify-center px-4">
        <div className="max-w-md text-center">
          <div className="mb-4 inline-flex size-14 items-center justify-center rounded-2xl border bg-card shadow-sm">
            <img src="/favicon.svg" alt="nomic" className="size-10" />
          </div>
          <h2 className="mb-1 text-lg font-semibold tracking-tight">向 nomic 提问</h2>
          <p className="mb-6 text-sm text-muted-foreground">
            可调用工具、读写文件、运行命令的 AI 编程助手
          </p>
          {onExample && (
            <div className="flex flex-wrap justify-center gap-2">
              {EXAMPLES.map((example) => (
                <button
                  key={example}
                  type="button"
                  onClick={() => onExample(example)}
                  className="rounded-full border px-3.5 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                >
                  {example}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>
    )
  }

  return (
    <div className="relative h-full">
      <div ref={scrollRef} onScroll={onScroll} className="h-full overflow-y-auto">
        <div className="mx-auto flex max-w-3xl flex-col gap-3 px-4 py-4">
          {items.map((item) => (
            <MessageItem key={item.id} item={item} />
          ))}
        </div>
      </div>
      {showJump && (
        <button
          type="button"
          onClick={scrollToBottom}
          className="absolute bottom-4 right-4 flex size-9 items-center justify-center rounded-full border bg-background/90 text-muted-foreground shadow-sm backdrop-blur transition-colors hover:text-foreground"
          aria-label="跳到最新消息"
          title="跳到最新消息"
        >
          <ArrowDown className="size-4" />
        </button>
      )}
    </div>
  )
}
