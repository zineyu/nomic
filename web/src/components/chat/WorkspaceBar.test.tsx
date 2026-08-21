// WorkspaceBar 测试：启动页输入框上方的工作区选择栏（无默认 workspace）。
// 已登记 workspace 以下拉选择；「使用其他目录…」切换为内联路径输入
// （回车确认、Esc 取消）；手动输入的目录不在登记列表时展示完整路径。

import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { WorkspaceBar } from './WorkspaceBar'

const workspaces = [
  { id: 'wa', path: '/home/zine/alpha', session_count: 2, last_active_at: null },
  { id: 'wb', path: '/home/zine/beta', session_count: 1, last_active_at: null },
]

function renderBar(overrides: Partial<Parameters<typeof WorkspaceBar>[0]> = {}) {
  return render(
    <WorkspaceBar workspaces={workspaces} value="" onChange={vi.fn()} {...overrides} />,
  )
}

describe('WorkspaceBar', () => {
  it('未选择时展示「选择工作区」占位', () => {
    renderBar()
    expect(screen.getByRole('button', { name: /选择工作区/ })).toBeInTheDocument()
  })

  it('选中 workspace 后展示其名称（路径最后一段）', () => {
    renderBar({ value: '/home/zine/alpha' })
    const trigger = screen.getByRole('button', { name: /alpha/ })
    expect(trigger).toHaveAttribute('title', '/home/zine/alpha')
  })

  it('下拉列出已登记 workspace，点击后回调其路径', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    renderBar({ onChange })

    await user.click(screen.getByRole('button', { name: /选择工作区/ }))
    const item = await screen.findByRole('menuitem', { name: /beta/ })
    await user.click(item)
    expect(onChange).toHaveBeenCalledWith('/home/zine/beta')
  })

  it('「使用其他目录…」切换为内联输入，回车提交自定义路径', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    renderBar({ onChange })

    await user.click(screen.getByRole('button', { name: /选择工作区/ }))
    await user.click(await screen.findByRole('menuitem', { name: /使用其他目录/ }))
    const input = screen.getByRole('textbox', { name: '工作区路径' })
    await user.type(input, '~/code/proj{Enter}')
    expect(onChange).toHaveBeenCalledWith('~/code/proj')
    // 提交后退出输入模式，回到下拉形态
    expect(screen.queryByRole('textbox', { name: '工作区路径' })).not.toBeInTheDocument()
  })

  it('内联输入 Esc 取消，不回调', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    renderBar({ onChange })

    await user.click(screen.getByRole('button', { name: /选择工作区/ }))
    await user.click(await screen.findByRole('menuitem', { name: /使用其他目录/ }))
    await user.type(screen.getByRole('textbox', { name: '工作区路径' }), '/nope{Escape}')
    expect(onChange).not.toHaveBeenCalled()
    expect(screen.queryByRole('textbox', { name: '工作区路径' })).not.toBeInTheDocument()
  })

  it('选中路径不在登记列表（手动输入）时展示完整路径', () => {
    renderBar({ value: '/home/zine/gamma' })
    // 触发器展示名称，旁边展示完整路径
    expect(screen.getByRole('button', { name: /gamma/ })).toBeInTheDocument()
    // 触发器与旁注均带完整路径 title
    expect(screen.getAllByTitle('/home/zine/gamma')).toHaveLength(2)
  })

  it('无已登记 workspace 时下拉仅提供「使用其他目录…」', async () => {
    const user = userEvent.setup()
    renderBar({ workspaces: [] })

    await user.click(screen.getByRole('button', { name: /选择工作区/ }))
    expect(await screen.findByRole('menuitem', { name: /使用其他目录/ })).toBeInTheDocument()
    expect(screen.getAllByRole('menuitem')).toHaveLength(1)
  })
})
