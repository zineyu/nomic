// QuestionModal 测试：填空 / 单选 / 多选 + 自定义填写流程。

import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { QuestionModal } from './QuestionModal'
import type { AskUserQuestion } from '@/lib/types'

const base: AskUserQuestion = {
  question: '继续下一步？',
  kind: 'single_choice',
  options: ['是', '否', '✏️ 其他（自定义填写）'],
}

describe('QuestionModal', () => {
  it('填空：输入后提交', async () => {
    const user = userEvent.setup()
    const onAnswer = vi.fn()
    render(
      <QuestionModal
        id="q1"
        question={{ ...base, kind: 'fill_in', options: [] }}
        onAnswer={onAnswer}
      />,
    )
    await user.type(screen.getByPlaceholderText(/输入回答/), '自定义答案')
    await user.click(screen.getByRole('button', { name: '提交' }))
    expect(onAnswer).toHaveBeenCalledWith('q1', {
      answers: ['自定义答案'],
      custom: '自定义答案',
    })
  })

  it('单选：选择选项提交', async () => {
    const user = userEvent.setup()
    const onAnswer = vi.fn()
    render(<QuestionModal id="q1" question={base} onAnswer={onAnswer} />)
    await user.click(screen.getByText('是'))
    await user.click(screen.getByRole('button', { name: '提交' }))
    expect(onAnswer).toHaveBeenCalledWith('q1', { answers: ['是'], custom: null })
  })

  it('单选 + 自定义填写', async () => {
    const user = userEvent.setup()
    const onAnswer = vi.fn()
    render(<QuestionModal id="q1" question={base} onAnswer={onAnswer} />)
    await user.click(screen.getByText('✏️ 其他（自定义填写）'))
    await user.type(screen.getByPlaceholderText(/填写自定义答案/), '红色')
    await user.click(screen.getByRole('button', { name: '提交' }))
    expect(onAnswer).toHaveBeenCalledWith('q1', { answers: ['红色'], custom: '红色' })
  })

  it('多选：勾选多个 + 自定义', async () => {
    const user = userEvent.setup()
    const onAnswer = vi.fn()
    render(
      <QuestionModal
        id="q1"
        question={{ ...base, kind: 'multiple_choice' }}
        onAnswer={onAnswer}
      />,
    )
    await user.click(screen.getByText('是'))
    await user.click(screen.getByText('否'))
    await user.click(screen.getByRole('button', { name: '提交' }))
    expect(onAnswer).toHaveBeenCalledWith('q1', { answers: ['是', '否'], custom: null })
  })

  it('未选择时提交按钮禁用', () => {
    render(<QuestionModal id="q1" question={base} onAnswer={vi.fn()} />)
    expect(screen.getByRole('button', { name: '提交' })).toBeDisabled()
  })
})
