// ProjectBar 测试：启动页输入框上方的项目选择栏（无默认 project）。
// 已登记 project 以下拉选择；「使用其他目录…」切换为内联路径输入
// （回车确认、Esc 取消）；手动输入的目录不在登记列表时展示完整路径。

import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { ProjectBar } from './ProjectBar'

const projects = [
  { id: 'wa', path: '/home/zine/alpha', session_count: 2, last_active_at: null },
  { id: 'wb', path: '/home/zine/beta', session_count: 1, last_active_at: null },
]

function renderBar(overrides: Partial<Parameters<typeof ProjectBar>[0]> = {}) {
  return render(
    <ProjectBar projects={projects} value="" onChange={vi.fn()} {...overrides} />,
  )
}

describe('ProjectBar', () => {
  it('未选择时展示「选择项目」占位', () => {
    renderBar()
    expect(screen.getByRole('button', { name: /选择项目/ })).toBeInTheDocument()
  })

  it('选中 project 后展示其名称（路径最后一段）', () => {
    renderBar({ value: '/home/zine/alpha' })
    const trigger = screen.getByRole('button', { name: /alpha/ })
    expect(trigger).toHaveAttribute('title', '/home/zine/alpha')
  })

  it('下拉列出已登记 project，点击后回调其路径', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    renderBar({ onChange })

    await user.click(screen.getByRole('button', { name: /选择项目/ }))
    const item = await screen.findByRole('menuitem', { name: /beta/ })
    await user.click(item)
    expect(onChange).toHaveBeenCalledWith('/home/zine/beta')
  })

  it('「使用其他目录…」切换为内联输入，回车提交自定义路径', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    renderBar({ onChange })

    await user.click(screen.getByRole('button', { name: /选择项目/ }))
    await user.click(await screen.findByRole('menuitem', { name: /使用其他目录/ }))
    const input = screen.getByRole('textbox', { name: '项目路径' })
    await user.type(input, '~/code/proj{Enter}')
    expect(onChange).toHaveBeenCalledWith('~/code/proj')
    // 提交后退出输入模式，回到下拉形态
    expect(screen.queryByRole('textbox', { name: '项目路径' })).not.toBeInTheDocument()
  })

  it('内联输入 Esc 取消，不回调', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    renderBar({ onChange })

    await user.click(screen.getByRole('button', { name: /选择项目/ }))
    await user.click(await screen.findByRole('menuitem', { name: /使用其他目录/ }))
    await user.type(screen.getByRole('textbox', { name: '项目路径' }), '/nope{Escape}')
    expect(onChange).not.toHaveBeenCalled()
    expect(screen.queryByRole('textbox', { name: '项目路径' })).not.toBeInTheDocument()
  })

  it('选中路径不在登记列表（手动输入）时展示完整路径', () => {
    renderBar({ value: '/home/zine/gamma' })
    // 触发器展示名称，旁边展示完整路径
    expect(screen.getByRole('button', { name: /gamma/ })).toBeInTheDocument()
    // 触发器与旁注均带完整路径 title
    expect(screen.getAllByTitle('/home/zine/gamma')).toHaveLength(2)
  })

  it('无已登记 project 时下拉仅提供「使用其他目录…」', async () => {
    const user = userEvent.setup()
    renderBar({ projects: [] })

    await user.click(screen.getByRole('button', { name: /选择项目/ }))
    expect(await screen.findByRole('menuitem', { name: /使用其他目录/ })).toBeInTheDocument()
    expect(screen.getAllByRole('menuitem')).toHaveLength(1)
  })
})
