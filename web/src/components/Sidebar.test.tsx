// Sidebar 测试：简洁胶囊风格会话列表。
//
// 每个 session 只展示标题，长标题被 CSS 截断但可通过 title 读取全文；
// 当前会话以主色背景高亮；侧栏与聊天区之间有右侧分隔边框。

import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

import { Sidebar } from './Sidebar'

const LONG_TITLE =
  '左侧session列表与右侧会话界面不够清晰明确，session名称被硬遮盖，需要调整布局与对比度让左右两侧边界分明'

const sessions = [
  { id: 'a', title: LONG_TITLE, cwd: '/tmp', first_message_at: null, last_message_at: 1786960000000, message_count: 101 },
  { id: 'b', title: null, cwd: '/tmp', first_message_at: null, last_message_at: 1786960000000, message_count: 3 },
]

function renderSidebar(overrides: Partial<Parameters<typeof Sidebar>[0]> = {}) {
  return render(
    <Sidebar
      sessions={sessions}
      currentSessionId="a"
      running={false}
      onNewSession={vi.fn()}
      onResume={vi.fn()}
      {...overrides}
    />,
  )
}

describe('Sidebar', () => {
  it('会话标题带完整 title 提示，长标题悬停可读全文', () => {
    renderSidebar()
    const title = screen.getByText(LONG_TITLE)
    expect(title).toHaveAttribute('title', LONG_TITLE)
    // 缺省标题回退为「新会话」
    expect(screen.getByText('新会话')).toHaveAttribute('title', '新会话')
    // 简洁风格：列表项不展示消息数、时间等元信息
    expect(screen.queryByText(/101 条消息/)).not.toBeInTheDocument()
  })

  it('侧栏根部带右侧分隔边框（与聊天区边界清晰）', () => {
    renderSidebar()
    const root = screen.getByText('会话').closest('div')
    expect(root?.parentElement).not.toBeNull()
    const container = root?.parentElement
    expect(container?.className).toContain('border-r')
    expect(container?.className).toContain('border-sidebar-border')
  })

  it('当前会话高亮并标记 aria-current', () => {
    renderSidebar()
    const active = screen.getByRole('button', { name: new RegExp(LONG_TITLE) })
    expect(active).toHaveAttribute('aria-current', 'page')
    expect(active.className).toContain('bg-sidebar-primary')
  })

  it('运行中当前会话显示脉冲指示点', () => {
    renderSidebar({ running: true })
    const active = screen.getByRole('button', { name: new RegExp(LONG_TITLE) })
    expect(active.querySelector('span[aria-hidden="true"]')).not.toBeNull()
  })

  it('不再展示工作目录与上下文用量', () => {
    renderSidebar()
    expect(screen.queryByText('/tmp')).not.toBeInTheDocument()
    expect(screen.queryByText(/上下文/)).not.toBeInTheDocument()
    expect(screen.queryByText(/tokens/)).not.toBeInTheDocument()
  })
})
