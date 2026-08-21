// `@mention` 与 `/` 命令的输入解析（纯逻辑，与 Rust 侧 crates/app/nomic-cli/src/mention.rs
// 及 web prompt handler 的斜杠命令解析对应）。
//
// mention 是输入草稿里的内联标记，支持两类：
// - `@skill:<name>`：引用一个已发现的 skill（名称即 skill 目录名）
// - `@file:<path>`：引用一个文件（相对 session workspace 或绝对路径）
//
// 补全弹层只负责填好标记文本；发送后由服务端展开有效标记（无效标记原样保留）。

/** skill mention 前缀 */
export const SKILL_PREFIX = '@skill:'
/** file mention 前缀 */
export const FILE_PREFIX = '@file:'

/** mention 片段：`@` 到光标的文本（不含空白），kind 为 null 表示还在类型候选阶段 */
export interface MentionFragment {
  /** mention 类型；null = `@` 后尚未输入完整类型前缀（如 `@sk`） */
  kind: 'skill' | 'file' | null
  /** `@` 到光标的完整片段 */
  fragment: string
  /** 类型前缀后的部分（`@skill:ru` → `ru`；类型阶段为 `@` 后的文本） */
  prefix: string
  /** `@` 在文本中的索引（接受候选时从此处替换） */
  start: number
}

/**
 * 光标位于文本末尾、文本以 `@` 收尾且 `@` 后无空白时，返回该 mention 片段；
 * 否则返回 null（与 Rust 侧 `mention_fragment` 同一口径：`@` 须在串首或
 * 前导空白之后，避免邮箱等场景误触发）。
 */
export function mentionFragment(text: string): MentionFragment | null {
  const at = text.lastIndexOf('@')
  if (at < 0) return null
  if (at > 0 && !/\s/.test(text[at - 1])) return null
  const fragment = text.slice(at)
  if (/\s/.test(fragment.slice(1))) return null

  if (fragment.startsWith(SKILL_PREFIX)) {
    return { kind: 'skill', fragment, prefix: fragment.slice(SKILL_PREFIX.length), start: at }
  }
  if (fragment.startsWith(FILE_PREFIX)) {
    return { kind: 'file', fragment, prefix: fragment.slice(FILE_PREFIX.length), start: at }
  }
  return { kind: null, fragment, prefix: fragment.slice(1), start: at }
}

/** 类型阶段的候选（`@` 后输入了 `skill`/`file` 的部分前缀时） */
export function mentionTypeCandidates(fragment: MentionFragment): string[] {
  if (fragment.kind !== null) return []
  const candidates: string[] = []
  if ('skill'.startsWith(fragment.prefix)) candidates.push(SKILL_PREFIX)
  if ('file'.startsWith(fragment.prefix)) candidates.push(FILE_PREFIX)
  return candidates
}

/** 斜杠命令定义（web 命令子集；执行由服务端 web prompt handler 完成） */
export interface SlashCommand {
  name: string
  usage: string
  description: string
}

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: '/compact', usage: '/compact [聚焦指令]', description: '压缩上下文' },
  { name: '/continue', usage: '/continue', description: '续跑上次运行' },
]

/**
 * 斜杠命令候选：输入以 `/` 开头且尚未输入空白（仍在键入命令名）时，
 * 返回名称前缀匹配的命令；否则返回空。
 */
export function commandCandidates(text: string): SlashCommand[] {
  if (!text.startsWith('/') || /\s/.test(text)) return []
  return SLASH_COMMANDS.filter((command) => command.name.startsWith(text))
}
