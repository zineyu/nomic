// gsap 动画适配层：业务组件不直接依赖 gsap API，统一经由本模块。
// - 尊重 prefers-reduced-motion：减动效环境（含无 matchMedia 的 jsdom 测试环境）
//   下所有动画即时落定，保证可访问性与测试确定性；
// - 提供两类原语：入场（fadeSlideIn / staggerFadeSlideIn）与高度折叠（animateHeight），
//   以及对应的 React hooks（useEntrance / useCollapse）。

import gsap from 'gsap'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
/**
 * 是否处于减动效环境。无 matchMedia（如 jsdom）时视为减动效：
 * 动画全部跳过，测试与 SSR 行为确定。
 */
export function prefersReducedMotion(): boolean {
  return (
    typeof window.matchMedia !== 'function' ||
    window.matchMedia('(prefers-reduced-motion: reduce)').matches
  )
}

interface FadeSlideOptions {
  /** 起始下移距离（px），默认 12 */
  y?: number
  /** 时长（秒），默认 0.35 */
  duration?: number
  /** 延迟（秒），默认 0 */
  delay?: number
}

/** 元素入场：淡入 + 自下方滑入。减动效下为 no-op。 */
export function fadeSlideIn(el: Element, options: FadeSlideOptions = {}): void {
  if (prefersReducedMotion()) return
  gsap.from(el, {
    opacity: 0,
    y: options.y ?? 12,
    duration: options.duration ?? 0.35,
    delay: options.delay ?? 0,
    ease: 'power2.out',
    clearProps: 'opacity,transform',
  })
}

/** 容器内匹配 selector 的子元素交错入场。减动效下为 no-op。 */
export function staggerFadeSlideIn(
  container: Element,
  selector: string,
  options: FadeSlideOptions & { stagger?: number } = {},
): void {
  if (prefersReducedMotion()) return
  gsap.from(container.querySelectorAll(selector), {
    opacity: 0,
    y: options.y ?? 12,
    duration: options.duration ?? 0.35,
    delay: options.delay ?? 0,
    stagger: options.stagger ?? 0.06,
    ease: 'power2.out',
    clearProps: 'opacity,transform',
  })
}

/**
 * 高度展开/收起动画。el 应为 overflow-hidden 的包裹容器。
 * 收起完成后调用 onDone（通常用于卸载内容）；减动效下立即调用。
 */
export function animateHeight(el: HTMLElement, open: boolean, onDone?: () => void): void {
  if (prefersReducedMotion()) {
    onDone?.()
    return
  }
  if (open) {
    gsap.fromTo(
      el,
      { height: 0, opacity: 0 },
      {
        height: el.scrollHeight,
        opacity: 1,
        duration: 0.25,
        ease: 'power2.out',
        overwrite: 'auto',
        onComplete: () => {
          gsap.set(el, { clearProps: 'height,opacity' })
          onDone?.()
        },
      },
    )
  } else {
    gsap.fromTo(
      el,
      { height: el.scrollHeight, opacity: 1 },
      {
        height: 0,
        opacity: 0,
        duration: 0.2,
        ease: 'power2.in',
        overwrite: 'auto',
        onComplete: () => onDone?.(),
      },
    )
  }
}

/** 入场动画 hook：组件挂载时执行一次 fadeSlideIn。 */
export function useEntrance<T extends HTMLElement>(options: FadeSlideOptions = {}) {
  const ref = useRef<T>(null)
  const { y, duration, delay } = options
  useEffect(() => {
    if (ref.current) fadeSlideIn(ref.current, { y, duration, delay })
    // 仅挂载时执行一次；options 变化不重新入场
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])
  return ref
}

/**
 * 折叠/展开动画 hook。
 * 内容在 open 或收起动画进行期间保持挂载；调用方将 ref 绑到
 * overflow-hidden 的包裹容器，并按 mounted 决定是否渲染内容。
 */
export function useCollapse<T extends HTMLElement>(open: boolean) {
  const ref = useRef<T>(null)
  // 收起动画进行中（仅非减动效环境置 true）
  const [animating, setAnimating] = useState(false)
  const [prevOpen, setPrevOpen] = useState(open)
  const skipFirst = useRef(true)
  const reduced = prefersReducedMotion()

  // render 期状态调整（官方推荐模式）：open 变 false 时先进入动画态，
  // 保证内容在收起动画播放期间保持挂载
  if (open !== prevOpen) {
    setPrevOpen(open)
    if (!open && !reduced) setAnimating(true)
  }

  useLayoutEffect(() => {
    // 首帧不播动画（初始状态直接落定）
    if (skipFirst.current) {
      skipFirst.current = false
      return
    }
    if (reduced) return
    const el = ref.current
    if (!el) return
    if (open) {
      animateHeight(el, true)
    } else {
      animateHeight(el, false, () => setAnimating(false))
    }
  }, [open, reduced])

  return { ref, mounted: open || animating }
}
