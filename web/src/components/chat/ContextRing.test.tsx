// ContextRing 用量分档颜色测试。

import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { TooltipProvider } from '@/components/ui/tooltip'
import { ContextRing } from './ContextRing'

// 渲染后取 SVG 根元素（颜色 class 挂在 svg 上）
function ringClass(tokens: number, window: number | null): string {
  render(
    <TooltipProvider delayDuration={0}>
      <ContextRing tokens={tokens} window={window} />
    </TooltipProvider>,
  )
  const svg = document.querySelector('svg[aria-hidden="true"]')
  expect(svg).not.toBeNull()
  return svg?.getAttribute('class') ?? ''
}

describe('ContextRing 用量分档', () => {
  it('无窗口信息时为低调灰', () => {
    expect(ringClass(100, null)).toContain('text-muted-foreground')
  })

  it('<=50% 低调灰', () => {
    expect(ringClass(50, 100)).toContain('text-muted-foreground')
  })

  it('50–65% 强调色', () => {
    expect(ringClass(60, 100)).toContain('text-primary')
  })

  it('65–80% 琥珀警示', () => {
    expect(ringClass(70, 100)).toContain('text-warning')
  })

  it('>80% 红色危险', () => {
    expect(ringClass(90, 100)).toContain('text-destructive')
  })

  it('tooltip 展示用量明细', () => {
    render(
      <TooltipProvider delayDuration={0}>
        <ContextRing tokens={700} window={1000} />
      </TooltipProvider>,
    )
    expect(screen.getByRole('img')).toHaveAttribute(
      'aria-label',
      '上下文：700 / 1,000 tokens (70%)',
    )
  })
})
