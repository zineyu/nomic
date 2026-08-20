// 会话分组：按 workspace 归属拆分全局会话列表，保持活跃度排序。

import { describe, expect, it } from 'vitest'

import { groupSessionsByWorkspace, groupSessionsWithWorkspaces, workspaceName } from './sessions'
import type { SessionSummary, WorkspaceSummary } from './types'

function session(id: string, workspace: string, lastMessageAt: number | null = null): SessionSummary {
  return {
    id,
    title: id,
    workspace_id: `w-${workspace}`,
    workspace,
    first_message_at: null,
    last_message_at: lastMessageAt,
    message_count: 1,
  }
}

describe('workspaceName', () => {
  it('取路径最后一段作为展示名', () => {
    expect(workspaceName('/home/zine/space/project/nomic')).toBe('nomic')
    expect(workspaceName('/tmp/')).toBe('tmp')
    expect(workspaceName('relative/dir')).toBe('dir')
  })

  it('无法提取时回退为原值', () => {
    expect(workspaceName('/')).toBe('/')
    expect(workspaceName('')).toBe('')
  })
})

describe('groupSessionsByWorkspace', () => {
  it('按 workspace 分组并保持组内顺序', () => {
    const groups = groupSessionsByWorkspace([
      session('a1', '/ws/a', 300),
      session('b1', '/ws/b', 200),
      session('a2', '/ws/a', 100),
    ])
    expect(groups.map((g) => g.workspace)).toEqual(['/ws/a', '/ws/b'])
    expect(groups[0].sessions.map((s) => s.id)).toEqual(['a1', 'a2'])
    expect(groups[1].sessions.map((s) => s.id)).toEqual(['b1'])
  })

  it('组顺序取各组最新会话的活跃度先后（输入已按活跃度排序）', () => {
    const groups = groupSessionsByWorkspace([
      session('b1', '/ws/b', 300),
      session('a1', '/ws/a', 200),
      session('b2', '/ws/b', 100),
    ])
    expect(groups.map((g) => g.workspace)).toEqual(['/ws/b', '/ws/a'])
  })

  it('空列表返回空分组', () => {
    expect(groupSessionsByWorkspace([])).toEqual([])
  })

  it('同路径不同写法的 workspace 归为两组（分组键不做路径规范化）', () => {
    const groups = groupSessionsByWorkspace([session('a', '/ws/a'), session('b', '/ws/a/')])
    expect(groups).toHaveLength(2)
  })
})

function ws(id: string, path: string): WorkspaceSummary {
  return { id, path, session_count: 0, last_active_at: null }
}

describe('groupSessionsWithWorkspaces', () => {
  it('无会话的 workspace 也展示为空组，组顺序以 workspace 列表为准', () => {
    const groups = groupSessionsWithWorkspaces(
      [ws('wb', '/ws/b'), ws('wc', '/ws/c'), ws('wa', '/ws/a')],
      [session('a1', '/ws/a'), session('b1', '/ws/b')],
    )
    expect(groups.map((g) => g.workspace)).toEqual(['/ws/b', '/ws/c', '/ws/a'])
    expect(groups[0].sessions.map((s) => s.id)).toEqual(['b1'])
    expect(groups[1].sessions).toEqual([])
    expect(groups[2].sessions.map((s) => s.id)).toEqual(['a1'])
  })

  it('会话所属 workspace 不在列表中时追加到末尾', () => {
    const groups = groupSessionsWithWorkspaces([ws('wa', '/ws/a')], [session('x1', '/ws/x')])
    expect(groups.map((g) => g.workspace)).toEqual(['/ws/a', '/ws/x'])
  })

  it('workspace 列表为空时退化为纯会话分组', () => {
    const groups = groupSessionsWithWorkspaces([], [session('a1', '/ws/a')])
    expect(groups.map((g) => g.workspace)).toEqual(['/ws/a'])
  })
})
