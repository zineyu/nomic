// Markdown 渲染：react-markdown + GFM，自定义代码块/链接/列表样式。
// 代码块通过 CodeBlock 组件接入 shiki 语法高亮 + 复制按钮。

import { isValidElement, memo, type ComponentProps } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

import { CodeBlock } from '@/components/CodeBlock'
import { cn } from '@/lib/utils'

type CodeProps = ComponentProps<'code'> & { node?: unknown }

function Code({ className, children, ...props }: CodeProps) {
  const isBlock = className?.includes('language-') ?? false
  if (isBlock) {
    return (
      <code className={cn('font-mono text-[0.85em]', className)} {...props}>
        {children}
      </code>
    )
  }
  return (
    <code
      className="rounded bg-muted px-1.5 py-0.5 font-mono text-[0.85em] text-foreground"
      {...props}
    >
      {children}
    </code>
  )
}

/** 从 code 子元素提取语言和原始文本 */
function extractCodeBlock(children: unknown): { lang: string; code: string } | null {
  if (!isValidElement(children)) return null
  const props = children.props as { className?: string; children?: unknown }
  const className = props.className ?? ''
  const match = className.match(/language-(\S+)/)
  if (!match) return null
  const lang = match[1]
  // children 是字符串（react-markdown 对 fenced code block 的行为）
  const code = typeof props.children === 'string' ? props.children.trimEnd() : ''
  if (!code) return null
  return { lang, code }
}

function Pre({ children }: ComponentProps<'pre'>) {
  const block = extractCodeBlock(children)
  if (block) {
    return <CodeBlock lang={block.lang} code={block.code} />
  }
  return (
    <pre className="my-3 overflow-x-auto rounded-lg border bg-muted/50 p-3 text-xs leading-relaxed">
      {children}
    </pre>
  )
}

function Link({ href, children }: ComponentProps<'a'>) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="text-primary underline underline-offset-2 hover:text-primary/80"
    >
      {children}
    </a>
  )
}

function Heading({ level, children }: ComponentProps<'h1'> & { level?: number }) {
  const sizes: Record<number, string> = {
    1: 'mb-3 mt-4 text-xl font-semibold',
    2: 'mb-2 mt-4 text-lg font-semibold',
    3: 'mb-2 mt-3 text-base font-semibold',
    4: 'mb-1 mt-2 text-base font-medium',
  }
  const Tag = `h${Math.min(Math.max(level ?? 2, 1), 4)}` as 'h1' | 'h2' | 'h3' | 'h4'
  return <Tag className={cn('text-foreground', sizes[level ?? 2])}>{children}</Tag>
}

function Table({ children }: ComponentProps<'table'>) {
  return (
    <div className="my-3 overflow-x-auto">
      <table className="w-full border-collapse text-xs">{children}</table>
    </div>
  )
}

function TableCell({
  isHeader,
  children,
  ...props
}: ComponentProps<'td'> & { isHeader?: boolean }) {
  const Tag = isHeader ? 'th' : 'td'
  return (
    <Tag
      className={cn(
        'border px-2 py-1 text-left align-top',
        isHeader && 'bg-muted/50 font-medium',
      )}
      {...props}
    >
      {children}
    </Tag>
  )
}

function MarkdownImpl({
  children,
  className,
}: {
  children: string
  className?: string
}) {
  return (
    <div className={cn('space-y-1 break-words text-base leading-relaxed', className)}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          code: Code,
          pre: Pre,
          a: Link,
          ul: ({ children }) => (
            <ul className="my-2 list-disc space-y-1 pl-6">{children}</ul>
          ),
          ol: ({ children }) => (
            <ol className="my-2 list-decimal space-y-1 pl-6">{children}</ol>
          ),
          h1: Heading,
          h2: Heading,
          h3: Heading,
          h4: Heading,
          h5: Heading,
          h6: Heading,
          table: Table,
          th: (props) => TableCell({ isHeader: true, ...props }),
          td: TableCell,
          hr: () => <hr className="my-3 border-border" />,
          blockquote: ({ children }) => (
            <blockquote className="my-2 border-l-2 border-primary/40 pl-3 text-muted-foreground">
              {children}
            </blockquote>
          ),
        }}
      >
        {children}
      </ReactMarkdown>
    </div>
  )
}

// memo：流式期间文本经 useThrottledValue 节流，children 稳定时跳过重解析。
export const Markdown = memo(MarkdownImpl)
