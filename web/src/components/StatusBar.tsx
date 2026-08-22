// 状态栏：显示会话统计信息（轮次、步骤、LLM 时间、工具时间、token 速率等）。

import type { SessionStats } from '@/lib/types'

function formatDuration(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000)
  const hours = Math.floor(totalSeconds / 3600)
  const minutes = Math.floor((totalSeconds % 3600) / 60)
  const seconds = totalSeconds % 60
  if (hours > 0) return `${hours}h${minutes}m${seconds}s`
  if (minutes > 0) return `${minutes}m${seconds}s`
  return `${seconds}s`
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}

interface StatusBarProps {
  stats: SessionStats
  running: boolean
}

export function StatusBar({ stats, running }: StatusBarProps) {
  if (stats.rounds === 0 && stats.total_steps === 0) return null

  return (
    <div className="flex items-center justify-center border-t border-border bg-muted/30 px-4 py-1.5 text-xs text-muted-foreground">
      <div className="flex flex-wrap items-center justify-center gap-x-3 gap-y-1">
        <span>
          {stats.rounds} 轮 · {stats.total_steps} 步
        </span>
        {stats.llm_time_ms > 0 && (
          <span>LLM {formatDuration(stats.llm_time_ms)}</span>
        )}
        {stats.tool_time_ms > 0 && (
          <span>· 工具调用 {formatDuration(stats.tool_time_ms)}</span>
        )}
        {stats.avg_first_token_ms > 0 && (
          <span>
            · 首 token 平均 {(stats.avg_first_token_ms / 1000).toFixed(1)}s
          </span>
        )}
        {stats.output_token_rate > 0 && (
          <span>· {Math.round(stats.output_token_rate)} tok/s</span>
        )}
        {stats.cache_hit_ratio > 0 && (
          <span>· 缓存命中 {Math.round(stats.cache_hit_ratio * 100)}%</span>
        )}
        {stats.input_tokens > 0 && (
          <span>· 输入 {formatTokens(stats.input_tokens)} tok</span>
        )}
        {stats.output_tokens > 0 && (
          <span>· 输出 {formatTokens(stats.output_tokens)} tok</span>
        )}
        {running && (
          <span className="flex items-center gap-1 text-foreground">
            <span className="relative flex size-1.5">
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-foreground opacity-75" />
              <span className="relative inline-flex size-1.5 rounded-full bg-foreground" />
            </span>
            运行中
          </span>
        )}
      </div>
    </div>
  )
}
