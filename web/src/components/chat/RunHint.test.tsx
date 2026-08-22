// RunHint 测试：阶段文案渲染、扫光样式类、空闲不渲染。

import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'

import { RunHint } from './RunHint'

describe('RunHint', () => {
  it('空闲（phase 为 null）不渲染', () => {
    const { container } = render(<RunHint phase={null} />)
    expect(container).toBeEmptyDOMElement()
  })

  it('渲染各阶段文案（同一风格）', () => {
    const { rerender } = render(<RunHint phase={{ kind: 'thinking' }} />)
    expect(screen.getByText('thinking...')).toBeInTheDocument()

    rerender(<RunHint phase={{ kind: 'writing' }} />)
    expect(screen.getByText('writing...')).toBeInTheDocument()

    rerender(<RunHint phase={{ kind: 'waiting' }} />)
    expect(screen.getByText('waiting...')).toBeInTheDocument()

    rerender(<RunHint phase={{ kind: 'tool', tool: 'bash' }} />)
    expect(screen.getByText('tool calling(bash)...')).toBeInTheDocument()
  })

  it('文案带扫光样式类（自左到右高亮动效）', () => {
    render(<RunHint phase={{ kind: 'thinking' }} />)
    expect(screen.getByText('thinking...')).toHaveClass('run-hint-shimmer')
  })
})
