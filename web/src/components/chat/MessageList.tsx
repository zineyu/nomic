// 消息列表：滚动容器 + 贴近底部时自动跟随新内容。

import { useEffect, useRef } from 'react'

import { MessageItem } from '@/components/chat/MessageItem'
import type { ChatItem } from '@/lib/chat'

export function MessageList({ items }: { items: ChatItem[] }) {
  const scrollRef = useRef<HTMLDivElement>(null)
  const stickToBottom = useRef(true)

  // 用户主动上滚时停止跟随，回到底部恢复
  const onScroll = () => {
    const el = scrollRef.current
    if (!el) return
    stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80
  }

  useEffect(() => {
    const el = scrollRef.current
    if (el && stickToBottom.current) {
      el.scrollTop = el.scrollHeight
    }
  }, [items])

  if (items.length === 0) {
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        <div className="text-center">
          <div className="mb-2 text-2xl">🦀</div>
          向 nomic 提问，开始新一轮对话
        </div>
      </div>
    )
  }

  return (
    <div ref={scrollRef} onScroll={onScroll} className="h-full overflow-y-auto">
      <div className="mx-auto flex max-w-3xl flex-col gap-3 px-4 py-4">
        {items.map((item) => (
          <MessageItem key={item.id} item={item} />
        ))}
      </div>
    </div>
  )
}
