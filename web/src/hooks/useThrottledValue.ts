// useThrottledValue：带 trailing 的节流值。delay <= 0 时直接返回最新值；
// 否则首帧立即反映，后续在 delay 间隔内合并更新、结束时冲刷到最新值。
// 用于流式 markdown：避免每个字符 delta 触发一次完整解析。
//
// 实现说明：所有 setState 都发生在 setTimeout 回调内，避免在 effect 体内
// 同步 setState（会触发级联渲染，见 react-hooks/set-state-in-effect）。

import { useEffect, useRef, useState } from 'react'

export function useThrottledValue<T>(value: T, delay: number): T {
  const [throttled, setThrottled] = useState(value)
  const lastEmit = useRef(0)
  const prevValue = useRef(value)

  useEffect(() => {
    if (delay <= 0) return
    // 值未变化（如挂载或无关重渲染）时跳过，保留首次变更的「立即反映」语义
    if (Object.is(prevValue.current, value)) return
    prevValue.current = value
    const now = Date.now()
    const remaining = Math.max(0, delay - (now - lastEmit.current))
    const id = window.setTimeout(() => {
      lastEmit.current = Date.now()
      setThrottled(value)
    }, remaining)
    return () => window.clearTimeout(id)
  }, [value, delay])

  return delay <= 0 ? value : throttled
}
