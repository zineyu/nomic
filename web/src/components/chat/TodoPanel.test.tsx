// TodoPanel 测试：空清单不渲染、标题与进度计数、默认折叠与点击展开、
// 完成/取消条目删除线置灰、嵌套子任务渲染。

import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'

import { TodoPanel } from './TodoPanel'
import { parseTodos } from '@/lib/todo'

const TODOS = parseTodos([
  {
    title: '搭建框架',
    status: 'completed',
    children: [{ title: '子任务', status: 'pending' }],
  },
  { title: '实现功能', status: 'in_progress' },
  { title: '编写测试', status: 'pending' },
  { title: '废弃方案', status: 'cancelled' },
])

describe('TodoPanel', () => {
  it('空清单不渲染', () => {
    const { container } = render(<TodoPanel todos={[]} />)
    expect(container.querySelector('[data-slot="todo-panel"]')).toBeNull()
  })

  it('渲染标题与进度计数（嵌套计入总数，cancelled 计入完成）', () => {
    render(<TodoPanel todos={TODOS} />)
    expect(screen.getByText('任务清单 · 2/5 已完成')).toBeInTheDocument()
  })

  it('默认折叠：清单不渲染，aria-expanded 为 false；点击后展开', async () => {
    render(<TodoPanel todos={TODOS} />)
    const toggle = screen.getByRole('button', { name: /任务清单/ })
    expect(toggle).toHaveAttribute('aria-expanded', 'false')
    expect(screen.queryByText('搭建框架')).not.toBeInTheDocument()

    await userEvent.click(toggle)
    expect(toggle).toHaveAttribute('aria-expanded', 'true')
    expect(screen.getByText('搭建框架')).toBeInTheDocument()
    expect(screen.getByText('实现功能')).toBeInTheDocument()
    expect(screen.getByText('子任务')).toBeInTheDocument()
  })

  it('完成/取消的条目删除线置灰，未完结条目正常显示', async () => {
    render(<TodoPanel todos={TODOS} />)
    await userEvent.click(screen.getByRole('button', { name: /任务清单/ }))
    const completed = screen.getByText('搭建框架')
    expect(completed.className).toContain('line-through')
    expect(completed.className).toContain('text-muted-foreground')

    const cancelled = screen.getByText('废弃方案')
    expect(cancelled.className).toContain('line-through')
    expect(cancelled.className).toContain('text-muted-foreground')

    const inProgress = screen.getByText('实现功能')
    expect(inProgress.className).not.toContain('line-through')

    const pending = screen.getByText('编写测试')
    expect(pending.className).not.toContain('line-through')
  })

  it('渲染各状态图标（无障碍标签）', async () => {
    render(<TodoPanel todos={TODOS} />)
    await userEvent.click(screen.getByRole('button', { name: /任务清单/ }))
    expect(screen.getByLabelText('已完成')).toBeInTheDocument()
    expect(screen.getByLabelText('已取消')).toBeInTheDocument()
    expect(screen.getByLabelText('进行中')).toBeInTheDocument()
    expect(screen.getAllByLabelText('未开始')).toHaveLength(2)
  })
})
