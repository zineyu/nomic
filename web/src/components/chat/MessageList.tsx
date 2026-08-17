// 消息列表：滚动容器 + 贴近底部时自动跟随新内容；上滚后显示「跳到最新」。
// 连续工具调用折叠：最多展示 2 个工具卡片，其余收进「已折叠 N 个工具调用」组。

import { useEffect, useRef, useState, type ReactNode } from 'react'
import { ArrowDown, Layers } from 'lucide-react'

import { MessageItem } from '@/components/chat/MessageItem'
import { ToolCard } from '@/components/chat/ToolCard'
import type { ChatItem, ToolItem } from '@/lib/chat'
import { cn } from '@/lib/utils'

const EXAMPLES = [
  '帮我看一下这个项目的整体结构',
  '用 Rust 写一个快速排序并解释复杂度',
  '给当前改动写一份 commit message',
]

/** 连续工具调用超过该数量时折叠 */
const MAX_VISIBLE_TOOLS = 2

function ToolGroup({ tools }: { tools: ToolItem[] }) {
  const [open, setOpen] = useState(false)
  return (
    <div className="flex max-w-[700px] flex-col gap-2">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex h-8 w-full items-center gap-2 rounded-lg border bg-card px-4 text-xs text-muted-foreground transition-colors hover:bg-accent/60"
      >
        <Layers className="size-3.5 shrink-0" />
        <span className="flex-1 text-left">
          已折叠 {tools.length} 个工具调用 · {tools.map((t) => t.name).join(' / ')}
        </span>
        <span className={cn('transition-transform', open && 'rotate-90')}>›</span>
      </button>
      {open && tools.map((tool) => <ToolCard key={tool.id} item={tool} />)}
    </div>
  )
}

/** 把 items 规整为渲染行：连续 tool 项折叠成组。 */
function renderRows(items: ChatItem[]): ReactNode[] {
  const rows: ReactNode[] = []
  let i = 0
  while (i < items.length) {
    const item = items[i]
    if (item.type !== 'tool') {
      rows.push(<MessageItem key={item.id} item={item} />)
      i += 1
      continue
    }
    let j = i
    while (j < items.length && items[j].type === 'tool') j += 1
    const run = items.slice(i, j) as ToolItem[]
    for (const tool of run.slice(0, MAX_VISIBLE_TOOLS)) {
      rows.push(<ToolCard key={tool.id} item={tool} />)
    }
    const collapsed = run.slice(MAX_VISIBLE_TOOLS)
    if (collapsed.length > 0) rows.push(<ToolGroup key={`group-${collapsed[0].id}`} tools={collapsed} />)
    i = j
  }
  return rows
}

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
      <div className="flex h-full items-center justify-center px-7">
        <div className="max-w-md text-center">
          <div className="mb-4 inline-flex size-14 items-center justify-center rounded-2xl bg-primary shadow-sm">
            <span className="text-xl font-bold text-primary-foreground">n</span>
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
                  className="rounded-full border border-border bg-card px-3.5 py-1.5 text-xs text-muted-foreground transition-colors hover:border-ring hover:text-foreground"
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
        <div className="mx-auto w-full max-w-[920px] px-7 py-5">
          <div className="flex flex-col gap-3.5">{renderRows(items)}</div>
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
