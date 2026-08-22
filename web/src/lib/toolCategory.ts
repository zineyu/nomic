// 工具类别：按行为分组，用于 ToolCard 图标的类别区分。
// 极简单色体系：不使用彩色类别色板，改用 foreground 明度阶梯——
// 墨色越重代表动作后果越强（执行 > 查看 > 修改 > 交互 > 代理）。
// 图标为装饰性元素；工具名保持 muted-foreground 以满足 AA 文本对比度。

export type ToolCategory = 'exec' | 'inspect' | 'modify' | 'interact' | 'agent' | 'other'

const CATEGORY_BY_TOOL: Record<string, ToolCategory> = {
  bash: 'exec',
  read: 'inspect',
  grep: 'inspect',
  find: 'inspect',
  write: 'modify',
  edit: 'modify',
  todo_read: 'interact',
  todo_write: 'interact',
  ask_user_question: 'interact',
  create_agent: 'agent',
  send_message: 'agent',
  wait_result: 'agent',
  wait_all: 'agent',
  close_agent: 'agent',
  list_agents: 'agent',
}

export function toolCategory(name: string): ToolCategory {
  return CATEGORY_BY_TOOL[name] ?? 'other'
}

const CATEGORY_ICON_CLASS: Record<ToolCategory, string> = {
  exec: 'text-foreground',
  inspect: 'text-foreground/75',
  modify: 'text-foreground/60',
  interact: 'text-foreground/45',
  agent: 'text-foreground/35',
  other: 'text-muted-foreground',
}

export function toolCategoryIconClass(name: string): string {
  return CATEGORY_ICON_CLASS[toolCategory(name)]
}
