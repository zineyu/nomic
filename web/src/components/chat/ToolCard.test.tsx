// ToolCard 与 briefArgs 测试。

import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { ToolCard } from './ToolCard'
import { briefArgs } from '@/lib/toolArgs'
import type { ToolItem } from '@/lib/chat'

describe('briefArgs', () => {
  it('提取关键字段（bash→command）', () => {
    expect(briefArgs('bash', { command: 'ls -la', timeout: 30 })).toBe('ls -la')
  })

  it('edit 多处编辑带计数', () => {
    expect(
      briefArgs('edit', { path: 'a.rs', edits: [{ oldText: 'x' }, { oldText: 'y' }] }),
    ).toBe('a.rs · 2 处编辑')
  })

  it('未知工具回退 JSON，多行压缩', () => {
    expect(briefArgs('web_search', { query: 'rust' })).toBe('{"query":"rust"}')
    expect(briefArgs('bash', { command: 'cargo build\n&& cargo test' })).toBe(
      'cargo build && cargo test',
    )
  })
})

function makeTool(overrides: Partial<ToolItem> = {}): ToolItem {
  return {
    type: 'tool',
    id: 't1',
    toolCallId: 't1',
    name: 'bash',
    args: { command: 'cargo test' },
    status: 'running',
    resultPreview: '',
    isError: false,
    ...overrides,
  }
}

describe('ToolCard', () => {
  it('渲染工具名与参数摘要', () => {
    render(<ToolCard item={makeTool()} />)
    expect(screen.getByText('bash')).toBeInTheDocument()
    expect(screen.getByText('cargo test')).toBeInTheDocument()
    expect(screen.getByText('执行中…')).toBeInTheDocument()
  })

  it('执行完成后点击展开显示结果', async () => {
    const user = userEvent.setup()
    render(
      <ToolCard item={makeTool({ status: 'done', resultPreview: 'ok', args: { command: 'ls' } })} />,
    )
    await user.click(screen.getByRole('button'))
    expect(screen.getByText('ok')).toBeInTheDocument()
  })

  it('错误工具显示错误态', () => {
    render(<ToolCard item={makeTool({ status: 'error', isError: true })} />)
    expect(screen.getByText('bash')).toBeInTheDocument()
  })
})
