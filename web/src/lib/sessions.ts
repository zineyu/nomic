// 会话列表的分组模型：按 workspace 归属把全局会话列表拆成分组视图。
//
// 服务端返回的列表按最近活跃排序（last_message_at DESC，NULL 最后）；
// 分组保持「组内顺序不变、组顺序取组内首个会话的出现顺序」，因此组的
// 先后即各组最新会话的活跃度先后，无需额外排序。

import type { SessionSummary } from './types'

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
