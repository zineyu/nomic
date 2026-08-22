// 工具类别：按行为分组，用于 ToolCard 图标的类别色。
// 类别色取自 chart-1~5 类别色板（见 DESIGN.md），仅用于装饰性图标；
// 工具名保持 muted-foreground 以满足 AA 文本对比度。

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
  exec: 'text-chart-1',
  inspect: 'text-chart-2',
  modify: 'text-chart-3',
  interact: 'text-chart-4',
  agent: 'text-chart-5',
  other: 'text-muted-foreground',
}

export function toolCategoryIconClass(name: string): string {
  return CATEGORY_ICON_CLASS[toolCategory(name)]
}
