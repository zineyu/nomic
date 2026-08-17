// useThrottledValue 测试：delay=0 同步；delay>0 变更经节流后冲刷。

import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { useThrottledValue } from './useThrottledValue'

describe('useThrottledValue', () => {
  beforeEach(() => {
    vi.useFakeTimers()
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  it('delay=0 时同步返回最新值', () => {
    const { result, rerender } = renderHook(
      ({ value, delay }: { value: string; delay: number }) =>
        useThrottledValue(value, delay),
      { initialProps: { value: 'a', delay: 0 } },
    )
    expect(result.current).toBe('a')
    rerender({ value: 'b', delay: 0 })
    expect(result.current).toBe('b')
  })

  it('delay>0 时变更先保持旧值，节流后冲刷到最新值', async () => {
    const { result, rerender } = renderHook(
      ({ value }: { value: string }) => useThrottledValue(value, 80),
      { initialProps: { value: 'a' } },
    )
    rerender({ value: 'b' })
    // setState 在 setTimeout 回调内，尚未触发
    expect(result.current).toBe('a')

    await act(async () => {
      vi.advanceTimersByTime(80)
    })
    expect(result.current).toBe('b')
  })
})
