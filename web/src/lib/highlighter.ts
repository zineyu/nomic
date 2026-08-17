// Shiki 高亮器单例：限定常用语言集 + github-light/github-dark 双主题。
// 首次调用时异步初始化，后续复用；未知语言回退纯文本。

import { createHighlighter, type Highlighter } from 'shiki'

const LANGS = [
  'rust',
  'typescript',
  'tsx',
  'javascript',
  'jsx',
  'python',
  'bash',
  'shell',
  'toml',
  'json',
  'jsonc',
  'markdown',
  'yaml',
  'go',
  'c',
  'cpp',
  'diff',
  'sql',
  'html',
  'css',
  'text',
] as const

export type SupportedLang = (typeof LANGS)[number]

const THEME_LIGHT = 'github-light'
const THEME_DARK = 'github-dark'

let highlighterPromise: Promise<Highlighter> | null = null

function getHighlighter(): Promise<Highlighter> {
  if (!highlighterPromise) {
    highlighterPromise = createHighlighter({
      themes: [THEME_LIGHT, THEME_DARK],
      langs: [...LANGS],
    })
  }
  return highlighterPromise
}

/** 语言是否在预加载集合中 */
export function isSupportedLang(lang: string | undefined): lang is SupportedLang {
  if (!lang) return false
  return (LANGS as readonly string[]).includes(lang)
}

/** 将语言别名规范化为 shiki 识别的语言 id */
export function normalizeLang(raw: string | undefined): string | undefined {
  if (!raw) return undefined
  const lower = raw.toLowerCase()
  const aliasMap: Record<string, string> = {
    sh: 'shell',
    zsh: 'shell',
    fish: 'shell',
    powershell: 'shell',
    ps1: 'shell',
    ts: 'typescript',
    tsx: 'tsx',
    js: 'javascript',
    jsx: 'jsx',
    py: 'python',
    rb: 'bash',
    yml: 'yaml',
    md: 'markdown',
    rs: 'rust',
    golang: 'go',
    'c++': 'cpp',
    csharp: 'text',
    cs: 'text',
    dockerfile: 'text',
    makefile: 'text',
  }
  return aliasMap[lower] ?? lower
}

export interface HighlightResult {
  html: string
  lang: string
}

/** 缓存：(lang, code) → html，限制 200 条 */
const cache = new Map<string, HighlightResult>()
const CACHE_LIMIT = 200

function cacheKey(lang: string, code: string): string {
  return `${lang}\u0000${code}`
}

function cacheSet(key: string, result: HighlightResult) {
  if (cache.size >= CACHE_LIMIT) {
    const first = cache.keys().next().value
    if (first !== undefined) cache.delete(first)
  }
  cache.set(key, result)
}

/**
 * 高亮代码。未知语言返回 null（调用方应降级为纯文本）。
 * 双主题：输出 HTML 使用 CSS 变量，需配合 .dark class 切换。
 */
export async function highlightCode(
  code: string,
  lang: string,
): Promise<HighlightResult | null> {
  const normalized = normalizeLang(lang)
  if (!normalized || !isSupportedLang(normalized)) return null

  const key = cacheKey(normalized, code)
  const cached = cache.get(key)
  if (cached) return cached

  try {
    const hl = await getHighlighter()
    const html = hl.codeToHtml(code, {
      lang: normalized,
      themes: { light: THEME_LIGHT, dark: THEME_DARK },
      defaultColor: false,
    })
    const result: HighlightResult = { html, lang: normalized }
    cacheSet(key, result)
    return result
  } catch {
    // 语言加载失败（理论上不应发生，因为已预加载）降级为纯文本
    return null
  }
}
