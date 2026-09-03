// 会话列表的分组模型：按 project 归属把全局会话列表拆成分组视图。
//
// 服务端返回的会话列表按最近活跃排序（last_message_at DESC，NULL 最后）；
// 纯会话分组保持「组内顺序不变、组顺序取组内首个会话的出现顺序」，因此组的
// 先后即各组最新会话的活跃度先后，无需额外排序。合并已登记 project 时
// （groupSessionsWithProjects）组顺序以 project 列表为准（登记时间升序，
// 稳定不随活跃度浮动），无会话的 project 展示为空组。

import type { SessionSummary, ProjectSummary } from './types'

export interface SessionGroup {
  /** 分组键：session 所属 project 的规范化路径 */
  project: string
  /** project 实体 id（删除 project 用；来自 project 列表或 session 摘要） */
  projectId: string
  /** 展示名：路径最后一段（空路径回退为原值） */
  name: string
  sessions: SessionSummary[]
}

/** 从 project 路径提取展示名（最后一段）。 */
export function projectName(path: string): string {
  return path.split('/').filter(Boolean).pop() ?? path
}

/** 按 project 分组；保持输入的活跃度排序（组内与组间）。 */
export function groupSessionsByProject(sessions: SessionSummary[]): SessionGroup[] {
  const groups = new Map<string, SessionGroup>()
  for (const session of sessions) {
    let group = groups.get(session.project)
    if (!group) {
      group = {
        project: session.project,
        projectId: session.project_id,
        name: projectName(session.project),
        sessions: [],
      }
      groups.set(session.project, group)
    }
    group.sessions.push(session)
  }
  return [...groups.values()]
}

/** 合并已登记的 project 与会话列表：无会话的 project 也展示为空组。
 *
 * 组顺序以 project 列表为准（服务端按登记时间升序，稳定不随活跃度浮动）；
 * project 列表为空（如 store 不可用）时退化为纯会话分组。 */
export function groupSessionsWithProjects(
  projects: ProjectSummary[],
  sessions: SessionSummary[],
): SessionGroup[] {
  if (projects.length === 0) return groupSessionsByProject(sessions)
  const groups = new Map<string, SessionGroup>()
  for (const ws of projects) {
    groups.set(ws.path, {
      project: ws.path,
      projectId: ws.id,
      name: projectName(ws.path),
      sessions: [],
    })
  }
  for (const session of sessions) {
    let group = groups.get(session.project)
    if (!group) {
      group = {
        project: session.project,
        projectId: session.project_id,
        name: projectName(session.project),
        sessions: [],
      }
      groups.set(session.project, group)
    }
    group.sessions.push(session)
  }
  return [...groups.values()]
}
