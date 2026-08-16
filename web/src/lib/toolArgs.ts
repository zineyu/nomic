// 工具参数摘要（对齐 Rust 侧 print.rs::brief_args 的口径：已知工具取关键
// 字段，其余回退 JSON；多行压缩、超长截断）。

export function briefArgs(toolName: string, args: Record<string, unknown>): string {
  const keyField: Record<string, string> = {
    bash: 'command',
    read: 'path',
    write: 'path',
    edit: 'path',
    grep: 'pattern',
    find: 'pattern',
    ask_user_question: 'question',
  }
  const field = keyField[toolName]
  let text: string
  if (toolName === 'edit' && field && typeof args[field] === 'string') {
    const edits = Array.isArray(args['edits']) ? args['edits'].length : 0
    text = edits > 1 ? `${args[field]} · ${edits} 处编辑` : String(args[field])
  } else if (field && typeof args[field] === 'string') {
    text = args[field] as string
  } else {
    text = JSON.stringify(args)
  }
  const squashed = text.split(/\s+/).join(' ')
  const max = 120
  return squashed.length <= max ? squashed : `${squashed.slice(0, max)}…`
}
