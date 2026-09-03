// work 分组：按 project 归属拆分全局 work 列表，保持活跃度排序。

import { describe, expect, it } from 'vitest'

import { groupWorksByProject, groupWorksWithProjects, projectName } from './works'
import type { WorkSummary, ProjectSummary } from './types'

function work(id: string, project: string, lastMessageAt: number | null = null): WorkSummary {
  return {
    id,
    main_session_id: `s-${id}`,
    title: id,
    project_id: `p-${project}`,
    project,
    session_count: 1,
    first_message_at: null,
    last_message_at: lastMessageAt,
    message_count: 1,
  }
}

describe('projectName', () => {
  it('取路径最后一段作为展示名', () => {
    expect(projectName('/home/zine/space/project/nomic')).toBe('nomic')
    expect(projectName('/tmp/')).toBe('tmp')
    expect(projectName('relative/dir')).toBe('dir')
  })

  it('无法提取时回退为原值', () => {
    expect(projectName('/')).toBe('/')
    expect(projectName('')).toBe('')
  })
})

describe('groupWorksByProject', () => {
  it('按 project 分组并保持组内顺序', () => {
    const groups = groupWorksByProject([
      work('a1', '/ws/a', 300),
      work('b1', '/ws/b', 200),
      work('a2', '/ws/a', 100),
    ])
    expect(groups.map((g) => g.project)).toEqual(['/ws/a', '/ws/b'])
    expect(groups[0].works.map((w) => w.id)).toEqual(['a1', 'a2'])
    expect(groups[1].works.map((w) => w.id)).toEqual(['b1'])
  })

  it('组顺序取各组最新 work 的活跃度先后（输入已按活跃度排序）', () => {
    const groups = groupWorksByProject([
      work('b1', '/ws/b', 300),
      work('a1', '/ws/a', 200),
      work('b2', '/ws/b', 100),
    ])
    expect(groups.map((g) => g.project)).toEqual(['/ws/b', '/ws/a'])
  })

  it('空列表返回空分组', () => {
    expect(groupWorksByProject([])).toEqual([])
  })

  it('同路径不同写法的 project 归为两组（分组键不做路径规范化）', () => {
    const groups = groupWorksByProject([work('a', '/ws/a'), work('b', '/ws/a/')])
    expect(groups).toHaveLength(2)
  })
})

function ws(id: string, path: string): ProjectSummary {
  return { id, path, session_count: 0, last_active_at: null }
}

describe('groupWorksWithProjects', () => {
  it('无 work 的 project 也展示为空组，组顺序以 project 列表为准', () => {
    const groups = groupWorksWithProjects(
      [ws('wb', '/ws/b'), ws('wc', '/ws/c'), ws('wa', '/ws/a')],
      [work('a1', '/ws/a'), work('b1', '/ws/b')],
    )
    expect(groups.map((g) => g.project)).toEqual(['/ws/b', '/ws/c', '/ws/a'])
    expect(groups[0].works.map((w) => w.id)).toEqual(['b1'])
    expect(groups[1].works).toEqual([])
    expect(groups[2].works.map((w) => w.id)).toEqual(['a1'])
  })

  it('work 所属 project 不在列表中时追加到末尾', () => {
    const groups = groupWorksWithProjects([ws('wa', '/ws/a')], [work('x1', '/ws/x')])
    expect(groups.map((g) => g.project)).toEqual(['/ws/a', '/ws/x'])
  })

  it('project 列表为空时退化为纯 work 分组', () => {
    const groups = groupWorksWithProjects([], [work('a1', '/ws/a')])
    expect(groups.map((g) => g.project)).toEqual(['/ws/a'])
  })
})
