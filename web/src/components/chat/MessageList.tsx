// 消息列表：滚动容器 + 贴近底部时自动跟随新内容；上滚后显示「跳到最新」。
// 连续工具调用折叠：最多展示 2 个工具卡片，其余收进「已折叠 N 个工具调用」组。

import { useEffect, useRef, useState, type ReactNode, type RefObject } from 'react'
import { ArrowDown, Layers } from 'lucide-react'

import { MessageItem } from '@/components/chat/MessageItem'
import { ToolCard } from '@/components/chat/ToolCard'
import { fadeSlideIn, staggerFadeSlideIn, useCollapse } from '@/lib/anim'
import type { ChatItem, ToolItem } from '@/lib/chat'
import { isEmptyAssistant } from '@/lib/chat'
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
  // 展开/收起时做高度动画；收起动画结束后卸载内容
  const { ref: listRef, mounted: listMounted } = useCollapse<HTMLDivElement>(open)
  return (
    <div className="flex flex-col gap-2">
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
      <div ref={listRef} className="overflow-hidden">
        {listMounted && (
          <div className="flex flex-col gap-2">
            {tools.map((tool) => (
              <ToolCard key={tool.id} item={tool} />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

/** 把 items 规整为渲染行：连续 tool 项折叠成组。 */
function renderRows(items: ChatItem[], initialBatch: RefObject<boolean>): ReactNode[] {
  const rows: ReactNode[] = []
  let i = 0
  while (i < items.length) {
    const item = items[i]
    // 渲染为空的 assistant 消息（无正文/思考的 tool_use 消息）直接跳过，
    // 否则空行占位会让两侧工具卡片出现双倍间距
    if (item.type === 'assistant' && isEmptyAssistant(item)) {
      i += 1
      continue
    }
    if (item.type !== 'tool') {
      rows.push(
        <EnterRow key={item.id} initialBatch={initialBatch}>
          <MessageItem item={item} />
        </EnterRow>,
      )
      i += 1
      continue
    }
    let j = i
    while (j < items.length && items[j].type === 'tool') j += 1
    const run = items.slice(i, j) as ToolItem[]
    for (const tool of run.slice(0, MAX_VISIBLE_TOOLS)) {
      rows.push(
        <EnterRow key={tool.id} initialBatch={initialBatch}>
          <ToolCard item={tool} />
        </EnterRow>,
      )
    }
    const collapsed = run.slice(MAX_VISIBLE_TOOLS)
    if (collapsed.length > 0) {
      rows.push(
        <EnterRow key={`group-${collapsed[0].id}`} initialBatch={initialBatch}>
          <ToolGroup tools={collapsed} />
        </EnterRow>,
      )
    }
    i = j
  }
  return rows
}

/**
 * 行入场动画容器：新追加的行 fade-up 入场；首个渲染批次（历史会话恢复）
 * 跳过动画。子 effect 先于父 effect 执行，故首批挂载时 initialBatch 仍为 true。
 */
function EnterRow({
  initialBatch,
  children,
}: {
  initialBatch: RefObject<boolean>
  children: ReactNode
}) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!initialBatch.current && ref.current) {
      fadeSlideIn(ref.current, { y: 10, duration: 0.3 })
    }
    // 仅挂载时判断是否入场
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])
  return <div ref={ref}>{children}</div>
}

/** 空会话启动页：logo / 标题 / 副文案 / 示例 chip 交错入场。 */
function EmptyState({ onExample }: { onExample?: (text: string) => void }) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (ref.current) {
      staggerFadeSlideIn(ref.current, '[data-intro]', { y: 14, stagger: 0.08 })
    }
  }, [])
  return (
    <div className="flex h-full items-center justify-center px-4 sm:px-7">
      <div ref={ref} className="max-w-md text-center">
        <img
          src="/favicon.svg"
          alt="nomic"
          data-intro
          className="mb-4 inline-block size-14 rounded-xl shadow-sm"
        />
        <h2 data-intro className="mb-1 text-h2 tracking-tight">
          向 nomic 提问
        </h2>
        <p data-intro className="mb-6 text-sm text-muted-foreground">
          可调用工具、读写文件、运行命令的 AI 编程助手
        </p>
        {onExample && (
          <div className="flex flex-wrap justify-center gap-2">
            {EXAMPLES.map((example) => (
              <button
                key={example}
                type="button"
                data-intro
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

/** 「跳到最新」浮动按钮：弹出式入场。 */
function JumpButton({ onClick }: { onClick: () => void }) {
  const ref = useRef<HTMLButtonElement>(null)
  useEffect(() => {
    if (ref.current) fadeSlideIn(ref.current, { y: 8, duration: 0.25 })
  }, [])
  return (
    <button
      ref={ref}
      type="button"
      onClick={onClick}
      className="absolute bottom-4 right-4 flex size-9 items-center justify-center rounded-full border bg-background/90 text-muted-foreground shadow-sm backdrop-blur transition-colors hover:text-foreground"
      aria-label="跳到最新消息"
      title="跳到最新消息"
    >
      <ArrowDown className="size-4" />
    </button>
  )
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
  // 首个渲染批次（历史会话恢复）跳过入场动画；子 effect 先于本 effect 执行
  const initialBatch = useRef(true)
  useEffect(() => {
    initialBatch.current = false
  }, [])

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
    return <EmptyState onExample={onExample} />
  }

  return (
    <div className="relative h-full">
      <div ref={scrollRef} onScroll={onScroll} className="h-full overflow-y-auto">
        <div className="mx-auto w-full max-w-page px-4 py-4 sm:px-7 sm:py-6">
          <div className="flex flex-col gap-4">{renderRows(items, initialBatch)}</div>
        </div>
      </div>
      {showJump && <JumpButton onClick={scrollToBottom} />}
    </div>
  )
}
