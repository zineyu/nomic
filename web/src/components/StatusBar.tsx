// 状态栏：模型/思考级别、上下文 token、运行状态、会话标题。

import { Loader2 } from 'lucide-react'

import { Badge } from '@/components/ui/badge'

interface StatusBarProps {
  model: string | null
  contextTokens: number
  running: boolean
  queued: number
  sessionTitle: string | null
}

export function StatusBar({ model, contextTokens, running, queued, sessionTitle }: StatusBarProps) {
  return (
    <div className="flex h-8 shrink-0 items-center gap-3 border-t bg-muted/30 px-4 text-[11px] text-muted-foreground">
      {running ? (
        <span className="flex items-center gap-1.5 text-primary">
          <Loader2 className="size-3 animate-spin" />
          运行中
        </span>
      ) : (
        <span className="text-muted-foreground/70">空闲</span>
      )}
      {queued > 0 && <Badge variant="secondary">队列 {queued}</Badge>}
      {model && (
        <span className="max-w-56 truncate" title={model}>
          模型：{model}
        </span>
      )}
      <span>上下文：{contextTokens.toLocaleString()} tokens</span>
      {sessionTitle && (
        <span className="ml-auto max-w-64 truncate" title={sessionTitle}>
          {sessionTitle}
        </span>
      )}
    </div>
  )
}
