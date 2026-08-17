// useTheme：主题切换（light/dark/system），localStorage 持久化 + matchMedia
// 跟随系统。挂载时读取偏好并同步应用 `.dark` class 到 documentElement。

import { useCallback, useEffect, useState } from 'react'

export type Theme = 'light' | 'dark' | 'system'

const STORAGE_KEY = 'nomic-theme'
const DARK_QUERY = '(prefers-color-scheme: dark)'

function getSystemDark(): boolean {
  return window.matchMedia(DARK_QUERY).matches
}

function resolveClass(theme: Theme): 'light' | 'dark' {
  if (theme === 'system') return getSystemDark() ? 'dark' : 'light'
  return theme
}

function applyTheme(theme: Theme) {
  const resolved = resolveClass(theme)
  document.documentElement.classList.toggle('dark', resolved === 'dark')
}

export function useTheme() {
  const [theme, setThemeState] = useState<Theme>(() => {
    const stored = localStorage.getItem(STORAGE_KEY)
    if (stored === 'light' || stored === 'dark' || stored === 'system') return stored
    return 'system'
  })

  // 应用主题到 DOM
  useEffect(() => {
    applyTheme(theme)
  }, [theme])

  // 监听系统主题变化（system 模式下跟随）
  useEffect(() => {
    if (theme !== 'system') return
    const mql = window.matchMedia(DARK_QUERY)
    const onChange = () => applyTheme('system')
    mql.addEventListener('change', onChange)
    return () => mql.removeEventListener('change', onChange)
  }, [theme])

  // 监听跨 tab 主题变化
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key !== STORAGE_KEY) return
      const v = e.newValue
      if (v === 'light' || v === 'dark' || v === 'system') setThemeState(v)
    }
    window.addEventListener('storage', onStorage)
    return () => window.removeEventListener('storage', onStorage)
  }, [])

  const setTheme = useCallback((next: Theme) => {
    setThemeState(next)
    localStorage.setItem(STORAGE_KEY, next)
    applyTheme(next)
  }, [])

  const cycle = useCallback(() => {
    const order: Theme[] = ['light', 'dark', 'system']
    const idx = order.indexOf(theme)
    setTheme(order[(idx + 1) % order.length])
  }, [theme, setTheme])

  return { theme, setTheme, cycle } as const
}
