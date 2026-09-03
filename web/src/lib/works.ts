// work 列表的分组模型：按 project 归属把全局 work 列表拆成分组视图
// （ADR-0044：work 是侧栏一等入口，点击打开其 main_session_id 主 session）。
//
// 服务端返回的 work 列表按最近活跃排序（last_message_at DESC，NULL 最后）；
// 纯 work 分组保持「组内顺序不变、组顺序取组内首个 work 的出现顺序」，因此组的
// 先后即各组最新 work 的活跃度先后，无需额外排序。合并已登记 project 时
// （groupWorksWithProjects）组顺序以 project 列表为准（登记时间升序，
// 稳定不随活跃度浮动），无 work 的 project 展示为空组。

import type { WorkSummary, ProjectSummary } from './types'

export interface WorkGroup {
  /** 分组键：work 所属 project 的规范化路径 */
  project: string
  /** project 实体 id（删除 project 用；来自 project 列表或 work 摘要） */
  projectId: string
  /** 展示名：路径最后一段（空路径回退为原值） */
  name: string
  works: WorkSummary[]
}

/** 从 project 路径提取展示名（最后一段）。 */
export function projectName(path: string): string {
  return path.split('/').filter(Boolean).pop() ?? path
}

/** 按 project 分组；保持输入的活跃度排序（组内与组间）。 */
export function groupWorksByProject(works: WorkSummary[]): WorkGroup[] {
  const groups = new Map<string, WorkGroup>()
  for (const work of works) {
    let group = groups.get(work.project)
    if (!group) {
      group = {
        project: work.project,
        projectId: work.project_id,
        name: projectName(work.project),
        works: [],
      }
      groups.set(work.project, group)
    }
    group.works.push(work)
  }
  return [...groups.values()]
}

/** 合并已登记的 project 与 work 列表：无 work 的 project 也展示为空组。
 *
 * 组顺序以 project 列表为准（服务端按登记时间升序，稳定不随活跃度浮动）；
 * project 列表为空（如 store 不可用）时退化为纯 work 分组。 */
export function groupWorksWithProjects(
  projects: ProjectSummary[],
  works: WorkSummary[],
): WorkGroup[] {
  if (projects.length === 0) return groupWorksByProject(works)
  const groups = new Map<string, WorkGroup>()
  for (const project of projects) {
    groups.set(project.path, {
      project: project.path,
      projectId: project.id,
      name: projectName(project.path),
      works: [],
    })
  }
  for (const work of works) {
    let group = groups.get(work.project)
    if (!group) {
      group = {
        project: work.project,
        projectId: work.project_id,
        name: projectName(work.project),
        works: [],
      }
      groups.set(work.project, group)
    }
    group.works.push(work)
  }
  return [...groups.values()]
}
