// anim 适配层测试：减动效（jsdom 默认无 matchMedia）下动画即时落定；
// 模拟非减动效时 gsap 立即应用起始帧。

import { describe, expect, it, vi, afterEach } from 'vitest'

import { animateHeight, fadeSlideIn, prefersReducedMotion, staggerFadeSlideIn } from './anim'

function stubMatchMedia(matches: boolean) {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  })) as unknown as typeof window.matchMedia
}

afterEach(() => {
  // @ts-expect-error 恢复 jsdom 默认（无 matchMedia）
  delete window.matchMedia
})

describe('prefersReducedMotion', () => {
  it('jsdom 无 matchMedia 时视为减动效', () => {
    expect(prefersReducedMotion()).toBe(true)
  })

  it('matchMedia 返回实际设置', () => {
    stubMatchMedia(true)
    expect(prefersReducedMotion()).toBe(true)
    stubMatchMedia(false)
    expect(prefersReducedMotion()).toBe(false)
  })
})

describe('fadeSlideIn', () => {
  it('减动效下不改动元素样式', () => {
    const el = document.createElement('div')
    fadeSlideIn(el)
    expect(el.getAttribute('style')).toBeNull()
  })

  it('非减动效下立即应用起始帧（透明 + 下移）', () => {
    stubMatchMedia(false)
    const el = document.createElement('div')
    document.body.appendChild(el)
    fadeSlideIn(el)
    expect(el.style.opacity).toBe('0')
    expect(el.style.transform).toContain('translate')
    el.remove()
  })
})

describe('staggerFadeSlideIn', () => {
  it('减动效下不改动子元素样式', () => {
    const box = document.createElement('div')
    box.innerHTML = '<span class="chip"></span><span class="chip"></span>'
    staggerFadeSlideIn(box, '.chip')
    for (const chip of box.querySelectorAll('.chip')) {
      expect(chip.getAttribute('style')).toBeNull()
    }
  })
})

describe('animateHeight', () => {
  it('减动效下收起立即回调 onDone', () => {
    const el = document.createElement('div')
    const onDone = vi.fn()
    animateHeight(el, false, onDone)
    expect(onDone).toHaveBeenCalledOnce()
  })

  it('减动效下展开不设置内联高度', () => {
    const el = document.createElement('div')
    animateHeight(el, true)
    expect(el.getAttribute('style')).toBeNull()
  })
})
