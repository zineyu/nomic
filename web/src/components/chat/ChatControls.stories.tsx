// ChatInput / QuestionModal / ToolCard stories。

import type { Meta, StoryObj } from '@storybook/react'

import { ChatInput } from '@/components/chat/ChatInput'
import { QuestionModal } from '@/components/chat/QuestionModal'
import { ToolCard } from '@/components/chat/ToolCard'

// ── ChatInput ────────────────────────────────────────────────────────────────

const chatInputMeta: Meta<typeof ChatInput> = {
  title: 'chat/ChatInput',
  component: ChatInput,
  parameters: { layout: 'padded' },
}

export default chatInputMeta
type InputStory = StoryObj<typeof ChatInput>

export const InputIdle: InputStory = {
  args: { running: false, queued: 0, onSend: () => {}, onStop: () => {} },
}

export const InputRunningWithQueue: InputStory = {
  args: { running: true, queued: 2, onSend: () => {}, onStop: () => {} },
}

// ── QuestionModal ────────────────────────────────────────────────────────────

export const QuestionSingleChoice: StoryObj<typeof QuestionModal> = {
  name: 'QuestionModal / 单选',
  render: () => (
    <QuestionModal
      id="q1"
      question={{
        question: '确认要删除这个分支吗？删除后原分支保留，可从会话树恢复。',
        kind: 'single_choice',
        options: ['确认删除', '取消', '✏️ 其他（自定义填写）'],
      }}
      onAnswer={() => {}}
    />
  ),
}

export const QuestionMultipleChoice: StoryObj<typeof QuestionModal> = {
  name: 'QuestionModal / 多选',
  render: () => (
    <QuestionModal
      id="q2"
      question={{
        question: '这次重构需要同时做什么？（可多选）',
        kind: 'multiple_choice',
        options: ['更新 README', '补充单测', '更新 CHANGELOG', '✏️ 其他（自定义填写）'],
      }}
      onAnswer={() => {}}
    />
  ),
}

export const QuestionFillIn: StoryObj<typeof QuestionModal> = {
  name: 'QuestionModal / 填空',
  render: () => (
    <QuestionModal
      id="q3"
      question={{ question: '请输入 release 版本号：', kind: 'fill_in', options: [] }}
      onAnswer={() => {}}
    />
  ),
}

// ── ToolCard ────────────────────────────────────────────────────────────────

export const ToolCardRunning: StoryObj<typeof ToolCard> = {
  name: 'ToolCard / 执行中',
  render: () => (
    <ToolCard
      item={{
        type: 'tool',
        id: '1',
        toolCallId: 't1',
        name: 'bash',
        args: { command: 'cargo nextest run --workspace --all-features --locked' },
        status: 'running',
        resultPreview: '',
        isError: false,
      }}
    />
  ),
}

export const ToolCardDone: StoryObj<typeof ToolCard> = {
  name: 'ToolCard / 完成',
  render: () => (
    <ToolCard
      item={{
        type: 'tool',
        id: '2',
        toolCallId: 't2',
        name: 'read',
        args: { path: 'Cargo.toml', offset: 1, limit: 200 },
        status: 'done',
        resultPreview:
          '[workspace]\nresolver = "3"\nmembers = ["crates/runtime/*", "crates/app/*"]\n\n[workspace.package]\nversion = "0.2.0"',
        isError: false,
      }}
    />
  ),
}

export const ToolCardError: StoryObj<typeof ToolCard> = {
  name: 'ToolCard / 失败',
  render: () => (
    <ToolCard
      item={{
        type: 'tool',
        id: '3',
        toolCallId: 't3',
        name: 'write',
        args: { path: '/etc/hosts', content: '…' },
        status: 'error',
        resultPreview: 'Permission denied (os error 13)',
        isError: true,
      }}
    />
  ),
}
