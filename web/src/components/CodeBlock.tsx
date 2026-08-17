// CodeBlock：异步语法高亮代码块（shiki 双主题）+ 复制按钮。
// 高亮前显示纯文本；未知语言降级为纯文本。

import { memo, useCallback, useEffect, useRef, useState } from 'react'
import { Check, Copy } from 'lucide-react'

import { highlightCode } from '@/lib/highlighter'

interface CodeBlockProps {
  lang?: string
  code: string
}

function CodeBlockImpl({ lang, code }: CodeBlockProps) {
  const [html, setHtml] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  const timerRef = useRef<ReturnType<typeof setTimeout>>(undefined)

  useEffect(() => {
    if (!lang || !code) return
    // debounce 120ms 以兼容流式增长
    timerRef.current = setTimeout(() => {
      void highlightCode(code, lang).then((result) => {
        if (result) setHtml(result.html)
      })
    }, 120)
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current)
    }
  }, [lang, code])

  const onCopy = useCallback(() => {
    void navigator.clipboard.writeText(code).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }, [code])

  return (
    <div className="group/code relative my-3">
      {html ? (
        <div
          className="shiki overflow-x-auto rounded-lg border bg-muted/50 p-3 text-xs leading-relaxed"
          dangerouslySetInnerHTML={{ __html: html }}
        />
      ) : (
        <pre className="overflow-x-auto rounded-lg border bg-muted/50 p-3 text-xs leading-relaxed">
          <code>{code}</code>
        </pre>
      )}
      <button
        type="button"
        onClick={onCopy}
        className="absolute top-2 right-2 flex size-7 items-center justify-center rounded-md border bg-background/80 text-muted-foreground opacity-0 backdrop-blur transition-opacity hover:text-foreground group-hover/code:opacity-100"
        aria-label={copied ? '已复制' : '复制代码'}
        title={copied ? '已复制' : '复制代码'}
      >
        {copied ? <Check className="size-3.5 text-success" /> : <Copy className="size-3.5" />}
      </button>
    </div>
  )
}

export const CodeBlock = memo(CodeBlockImpl)
