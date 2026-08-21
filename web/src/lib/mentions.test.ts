import { describe, expect, it } from 'vitest'

import {
  FILE_PREFIX,
  SKILL_PREFIX,
  commandCandidates,
  mentionFragment,
  mentionTypeCandidates,
} from './mentions'

describe('mentionFragment', () => {
  it('识别 mention 边界与片段', () => {
    expect(mentionFragment('用 @')).toEqual({
      kind: null,
      fragment: '@',
      prefix: '',
      start: 2,
    })
    expect(mentionFragment('@skill:ju')).toEqual({
      kind: 'skill',
      fragment: '@skill:ju',
      prefix: 'ju',
      start: 0,
    })
    expect(mentionFragment('看看 @file:src/ma')).toEqual({
      kind: 'file',
      fragment: '@file:src/ma',
      prefix: 'src/ma',
      start: 3,
    })
  })

  it('非 mention 场景返回 null', () => {
    expect(mentionFragment('没有 at 符号')).toBeNull()
    // `@` 后出现空白则视为普通文本
    expect(mentionFragment('@skill:ju ')).toBeNull()
    // 前导非空白不构成 mention 边界（如邮箱）
    expect(mentionFragment('a@b')).toBeNull()
  })

  it('类型阶段保留 `@` 后的部分前缀', () => {
    const fragment = mentionFragment('@sk')
    expect(fragment).toEqual({ kind: null, fragment: '@sk', prefix: 'sk', start: 0 })
  })
})

describe('mentionTypeCandidates', () => {
  it('按 `@` 后前缀过滤类型候选', () => {
    expect(mentionTypeCandidates(mentionFragment('@')!)).toEqual([SKILL_PREFIX, FILE_PREFIX])
    expect(mentionTypeCandidates(mentionFragment('@sk')!)).toEqual([SKILL_PREFIX])
    expect(mentionTypeCandidates(mentionFragment('@fi')!)).toEqual([FILE_PREFIX])
    expect(mentionTypeCandidates(mentionFragment('@xyz')!)).toEqual([])
    // 已进入具体 mention 类型后不再给类型候选
    expect(mentionTypeCandidates(mentionFragment('@skill:ju')!)).toEqual([])
  })
})

describe('commandCandidates', () => {
  it('按 `/` 前缀匹配命令', () => {
    expect(commandCandidates('/').map((c) => c.name)).toEqual(['/compact', '/continue'])
    expect(commandCandidates('/com').map((c) => c.name)).toEqual(['/compact'])
    expect(commandCandidates('/continue').map((c) => c.name)).toEqual(['/continue'])
    expect(commandCandidates('/xyz')).toEqual([])
  })

  it('非命令输入返回空', () => {
    expect(commandCandidates('')).toEqual([])
    expect(commandCandidates('你好')).toEqual([])
    // 已输入空白（进入参数阶段）不再弹命令候选
    expect(commandCandidates('/compact 指令')).toEqual([])
  })
})
