// 会话列表的分组模型：按 workspace 归属把全局会话列表拆成分组视图。
//
// 服务端返回的会话列表按最近活跃排序（last_message_at DESC，NULL 最后）；
// 纯会话分组保持「组内顺序不变、组顺序取组内首个会话的出现顺序」，因此组的
// 先后即各组最新会话的活跃度先后，无需额外排序。合并已登记 workspace 时
// （groupSessionsWithWorkspaces）组顺序以 workspace 列表为准（last_active
// 降序），无会话的 workspace 展示为空组。

import type { SessionSummary, WorkspaceSummary } from './types'

export interface SessionGroup {
  /** 分组键：session 所属 workspace 的规范化路径 */
  workspace: string
  /** 展示名：路径最后一段（空路径回退为原值） */
  name: string
  sessions: SessionSummary[]
}

/** 从 workspace 路径提取展示名（最后一段）。 */
export function workspaceName(path: string): string {
  return path.split('/').filter(Boolean).pop() ?? path
}

/** 按 workspace 分组；保持输入的活跃度排序（组内与组间）。 */
export function groupSessionsByWorkspace(sessions: SessionSummary[]): SessionGroup[] {
  const groups = new Map<string, SessionGroup>()
  for (const session of sessions) {
    let group = groups.get(session.workspace)
    if (!group) {
      group = {
        workspace: session.workspace,
        name: workspaceName(session.workspace),
        sessions: [],
      }
      groups.set(session.workspace, group)
    }
    group.sessions.push(session)
  }
  return [...groups.values()]
}

/** 合并已登记的 workspace 与会话列表：无会话的 workspace 也展示为空组。
 *
 * 组顺序以 workspace 列表为准（服务端已按 last_active 降序，无活动的排最后）；
 * workspace 列表为空（如 store 不可用）时退化为纯会话分组。 */
export function groupSessionsWithWorkspaces(
  workspaces: WorkspaceSummary[],
  sessions: SessionSummary[],
): SessionGroup[] {
  if (workspaces.length === 0) return groupSessionsByWorkspace(sessions)
  const groups = new Map<string, SessionGroup>()
  for (const ws of workspaces) {
    groups.set(ws.path, { workspace: ws.path, name: workspaceName(ws.path), sessions: [] })
  }
  for (const session of sessions) {
    let group = groups.get(session.workspace)
    if (!group) {
      group = {
        workspace: session.workspace,
        name: workspaceName(session.workspace),
        sessions: [],
      }
      groups.set(session.workspace, group)
    }
    group.sessions.push(session)
  }
  return [...groups.values()]
}
