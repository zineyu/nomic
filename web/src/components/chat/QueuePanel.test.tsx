// QueuePanel 测试：条目渲染（原文 + 附件数）、就地编辑保存/取消/空文本删除、
// 删除与上移/下移回调、编辑中条目消失时退出编辑子状态。

import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { QueuePanel } from './QueuePanel'
import type { QueueEntry } from '@/lib/types'

const ENTRIES: QueueEntry[] = [
  { id: '1', text: '第一条', images: 0 },
  { id: '2', text: '第二条', images: 2 },
  { id: '3', text: '第三条', images: 0 },
]

function renderPanel(props: Partial<Parameters<typeof QueuePanel>[0]> = {}) {
  return render(
    <QueuePanel
      queue={ENTRIES}
      onUpdate={vi.fn()}
      onRemove={vi.fn()}
      onMove={vi.fn()}
      {...props}
    />,
  )
}

describe('QueuePanel', () => {
  it('空队列不渲染', () => {
    const { container } = renderPanel({ queue: [] })
    expect(container.querySelector('[data-slot="queue-panel"]')).toBeNull()
  })

  it('渲染条目原文与附件数，首条禁用上移、末条禁用下移', () => {
    renderPanel()
    expect(screen.getByText(/已排队 3 条/)).toBeInTheDocument()
    expect(screen.getByText('第一条')).toBeInTheDocument()
    expect(screen.getByText('[图片 ×2]')).toBeInTheDocument()
    const ups = screen.getAllByTitle('上移')
    const downs = screen.getAllByTitle('下移')
    expect(ups[0]).toBeDisabled()
    expect(downs[2]).toBeDisabled()
    expect(ups[1]).toBeEnabled()
  })

  it('上移/下移/删除按条目 id 回调', async () => {
    const user = userEvent.setup()
    const onMove = vi.fn()
    const onRemove = vi.fn()
    renderPanel({ onMove, onRemove })

    await user.click(screen.getAllByTitle('下移')[0])
    expect(onMove).toHaveBeenCalledWith('1', 'down')

    await user.click(screen.getAllByTitle('上移')[2])
    expect(onMove).toHaveBeenCalledWith('3', 'up')

    await user.click(screen.getAllByTitle('删除')[1])
    expect(onRemove).toHaveBeenCalledWith('2')
  })

  it('点击编辑进入就地编辑，Enter 保存原文', async () => {
    const user = userEvent.setup()
    const onUpdate = vi.fn()
    renderPanel({ onUpdate })

    await user.click(screen.getAllByTitle('编辑')[1])
    const editor = screen.getByDisplayValue('第二条')
    await user.clear(editor)
    await user.type(editor, '改后的第二条')
    await user.keyboard('{Enter}')
    expect(onUpdate).toHaveBeenCalledWith('2', '改后的第二条')
    // 保存后退出编辑子状态
    expect(screen.queryByDisplayValue('改后的第二条')).not.toBeInTheDocument()
  })

  it('Esc 取消编辑不回填', async () => {
    const user = userEvent.setup()
    const onUpdate = vi.fn()
    renderPanel({ onUpdate })

    await user.click(screen.getAllByTitle('编辑')[0])
    const editor = screen.getByDisplayValue('第一条')
    await user.type(editor, '改动')
    await user.keyboard('{Escape}')
    expect(onUpdate).not.toHaveBeenCalled()
    expect(screen.getByText('第一条')).toBeInTheDocument()
  })

  it('空文本保存 = 删除该条目（空文本原样交给服务端）', async () => {
    const user = userEvent.setup()
    const onUpdate = vi.fn()
    renderPanel({ onUpdate })

    await user.click(screen.getAllByTitle('编辑')[0])
    await user.clear(screen.getByDisplayValue('第一条'))
    await user.keyboard('{Enter}')
    expect(onUpdate).toHaveBeenCalledWith('1', '')
  })

  it('编辑中的条目从队列消失（被注入/删除）时退出编辑子状态', async () => {
    const user = userEvent.setup()
    const { rerender } = render(
      <QueuePanel
        queue={ENTRIES}
        onUpdate={vi.fn()}
        onRemove={vi.fn()}
        onMove={vi.fn()}
      />,
    )
    await user.click(screen.getAllByTitle('编辑')[0])
    expect(screen.getByDisplayValue('第一条')).toBeInTheDocument()

    // 服务端广播：第一条已被注入（队列剩 2、3）
    rerender(
      <QueuePanel
        queue={ENTRIES.slice(1)}
        onUpdate={vi.fn()}
        onRemove={vi.fn()}
        onMove={vi.fn()}
      />,
    )
    expect(screen.queryByDisplayValue('第一条')).not.toBeInTheDocument()
    expect(screen.getByText('第二条')).toBeInTheDocument()
  })
})
