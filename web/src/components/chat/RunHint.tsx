// 运行状态提示：输入框上方的单行阶段提示（thinking... / tool calling... /
// writing... / waiting...），文字带自左到右扫过的高亮动效（CSS 渐变扫光，
// 见 index.css 的 .run-hint-shimmer）。与 TUI 的运行提示行同一口径
//（crates/app/nomic-cli/src/tui/widgets/runhint.rs）。

import { runHintText, type RunPhase } from '@/lib/chat'

interface RunHintProps {
  /** 运行阶段（`null` = 空闲，不渲染） */
  phase: RunPhase | null
}

export function RunHint({ phase }: RunHintProps) {
  if (!phase) return null
  return (
    <div data-slot="run-hint" className="mx-auto w-full max-w-page px-7 pb-0.5">
      <span className="run-hint-shimmer font-mono text-xs">{runHintText(phase)}</span>
    </div>
  )
}
