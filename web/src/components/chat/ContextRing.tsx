// 上下文用量环形指示器。
//
// 以小型 SVG 圆环直观展示上下文占用比例，常态不显示具体 token 数与
// 百分比；鼠标悬停时通过 Tooltip 展开详细信息。

import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { cn } from '@/lib/utils'

interface ContextRingProps {
  tokens: number
  window: number | null
  size?: number
  stroke?: number
  className?: string
}

export function ContextRing({
  tokens,
  window,
  size = 16,
  stroke = 2.5,
  className,
}: ContextRingProps) {
  const hasWindow = window !== null && window > 0
  const pct = hasWindow ? Math.min((tokens / window) * 100, 100) : null

  const radius = (size - stroke) / 2
  const circumference = 2 * Math.PI * radius
  const dashoffset = pct !== null ? circumference * (1 - pct / 100) : circumference

  const colorClass =
    pct === null
      ? 'text-muted-foreground'
      : pct > 80
        ? 'text-destructive'
        : pct > 50
          ? 'text-primary'
          : 'text-muted-foreground'

  const tooltip =
    pct !== null && window !== null
      ? `上下文：${tokens.toLocaleString()} / ${window.toLocaleString()} tokens (${pct.toFixed(0)}%)`
      : `上下文：${tokens.toLocaleString()} tokens`

  return (
    <Tooltip delayDuration={150}>
      <TooltipTrigger asChild>
        <span
          className={cn('inline-flex items-center justify-center', className)}
          aria-label={tooltip}
          role="img"
        >
          <svg
            width={size}
            height={size}
            viewBox={`0 0 ${size} ${size}`}
            className={cn('-rotate-90 transition-colors', colorClass)}
            aria-hidden="true"
          >
            {/* 背景环 */}
            <circle
              cx={size / 2}
              cy={size / 2}
              r={radius}
              fill="none"
              stroke="currentColor"
              strokeWidth={stroke}
              className="opacity-20"
            />
            {/* 进度环 */}
            <circle
              cx={size / 2}
              cy={size / 2}
              r={radius}
              fill="none"
              stroke="currentColor"
              strokeWidth={stroke}
              strokeLinecap="round"
              strokeDasharray={circumference}
              strokeDashoffset={dashoffset}
              className="transition-all duration-300"
            />
          </svg>
        </span>
      </TooltipTrigger>
      <TooltipContent side="top" sideOffset={4}>
        <p>{tooltip}</p>
      </TooltipContent>
    </Tooltip>
  )
}
