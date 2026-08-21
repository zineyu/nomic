// 输入区：模仿 DeepSeek Harness 布局。
// 输入框占满剩余空间，右下角模型选择器 + 发送按钮。
//
// 补全弹层（`@` mention 与 `/` 命令）：
// - `@` 触发类型候选（@skill: / @file:）；`@skill:` 后为 skill 名候选
//   （进程级清单，懒加载缓存）；`@file:` 后为文件候选（服务端按 session
//   workspace 前缀匹配，防抖查询）
// - `/` 开头触发斜杠命令候选（/compact、/continue；执行在服务端，
//   见 crates/app/nomic-cli/src/web/api/handlers.rs）
// - 弹层打开时 ↑/↓ 选择、Tab/Enter 接受、Esc 关闭；弹层关闭时 Enter 发送

import { useEffect, useRef, useState } from 'react'
import { SendHorizontal, Square } from 'lucide-react'

import { ContextRing } from '@/components/chat/ContextRing'
import { ModelPicker } from '@/components/ModelPicker'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { api } from '@/lib/api'
import {
  FILE_PREFIX,
  SKILL_PREFIX,
  commandCandidates,
  mentionFragment,
  mentionTypeCandidates,
} from '@/lib/mentions'
import type { SkillSummary } from '@/lib/types'
import { cn } from '@/lib/utils'

const MAX_LINES = 8
const LINE_HEIGHT = 24
const MAX_HEIGHT = MAX_LINES * LINE_HEIGHT

/** 文件候选查询防抖间隔（毫秒） */
const FILE_QUERY_DEBOUNCE_MS = 120

/** 弹层候选项 */
interface Candidate {
  /** 接受后写入输入框替换片段的文本 */
  value: string
  /** 主展示文本 */
  label: string
  /** 次级说明（skill 描述 / 命令用法） */
  hint?: string
  /** 完整候选（接受后补尾随空格结束该标记）；类型候选为 false（继续键入） */
  complete: boolean
}

/** 弹层状态：候选列表 + 选中索引 + 片段起点（接受时从此处替换到文本末尾） */
interface PopupState {
  candidates: Candidate[]
  selected: number
  start: number
}

interface ChatInputProps {
  running: boolean
  queued: number
  modelSpec?: string | null
  reasoning?: string | null
  contextTokens?: number
  contextWindow?: number | null
  /** 当前 session id（`@file:` 候选查询的 workspace 基准；无 session 时文件补全不可用） */
  sessionId?: string | null
  /** 额外禁用发送（启动页未选择工作区时） */
  sendDisabled?: boolean
  /** 输入框占位文本（缺省为「给智能体发消息」） */
  placeholder?: string
  onSwitchModel?: (spec: string, reasoning?: string) => void
  onSend: (text: string) => void
  onStop: () => void
}

export function ChatInput({
  running,
  queued,
  modelSpec,
  reasoning,
  contextTokens = 0,
  contextWindow = null,
  sessionId = null,
  sendDisabled = false,
  placeholder = '给智能体发消息',
  onSwitchModel,
  onSend,
  onStop,
}: ChatInputProps) {
  const [value, setValue] = useState('')
  const [popup, setPopup] = useState<PopupState | null>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  // skill 清单缓存（进程级，session 内不变；null = 尚未加载）
  const skillsRef = useRef<SkillSummary[] | null>(null)
  // 弹层重算序号：丢弃过期的异步结果（文件查询 / skill 加载）
  const popupSeq = useRef(0)

  // 输入自动增高（上限 MAX_LINES 行）
  useEffect(() => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, MAX_HEIGHT)}px`
  }, [value])

  // 按当前输入重算补全弹层（仅光标在末尾的场景，与 TUI 口径一致）
  useEffect(() => {
    const el = textareaRef.current
    const cursorAtEnd = !el || el.selectionStart === value.length
    const seq = ++popupSeq.current

    const apply = (candidates: Candidate[], start: number) => {
      if (popupSeq.current !== seq) return
      setPopup(candidates.length > 0 ? { candidates, selected: 0, start } : null)
    }

    if (!cursorAtEnd || !value) {
      apply([], 0)
      return
    }

    // `/` 命令候选（同步）
    if (value.startsWith('/')) {
      const commands = commandCandidates(value)
      apply(
        commands.map((command) => ({
          value: command.name,
          label: command.usage,
          hint: command.description,
          complete: true,
        })),
        0,
      )
      return
    }

    const fragment = mentionFragment(value)
    if (!fragment) {
      apply([], 0)
      return
    }

    // 类型候选（`@` 后尚未输入完整类型前缀）
    if (fragment.kind === null) {
      apply(
        mentionTypeCandidates(fragment).map((prefix) => ({
          value: prefix,
          label: prefix,
          hint: prefix === SKILL_PREFIX ? '引用 skill' : '引用文件',
          complete: false,
        })),
        fragment.start,
      )
      return
    }

    // `@skill:` 名候选（懒加载进程级清单，之后本地前缀过滤）
    if (fragment.kind === 'skill') {
      const match = (skills: SkillSummary[]) =>
        skills
          .filter((skill) => skill.name.startsWith(fragment.prefix))
          .map((skill) => ({
            value: `${SKILL_PREFIX}${skill.name}`,
            label: `${SKILL_PREFIX}${skill.name}`,
            hint: skill.description,
            complete: true,
          }))
      if (skillsRef.current) {
        apply(match(skillsRef.current), fragment.start)
      } else {
        void api
          .skills()
          .then((skills) => {
            skillsRef.current = skills
            apply(match(skills), fragment.start)
          })
          .catch(() => apply([], 0))
      }
      return
    }

    // `@file:` 路径候选（服务端按 session workspace 前缀匹配，防抖）
    if (!sessionId) {
      apply([], 0)
      return
    }
    const timer = setTimeout(() => {
      void api
        .files(sessionId, fragment.prefix)
        .then((files) =>
          apply(
            files.map((path) => ({
              value: `${FILE_PREFIX}${path}`,
              label: `${FILE_PREFIX}${path}`,
              complete: true,
            })),
            fragment.start,
          ),
        )
        .catch(() => apply([], 0))
    }, FILE_QUERY_DEBOUNCE_MS)
    return () => clearTimeout(timer)
  }, [value, sessionId])

  // 弹层选中项跟随滚动（键盘导航到可视区外时；jsdom 无 scrollIntoView，守卫）
  useEffect(() => {
    if (!popup) return
    const el = document.querySelector(
      `[data-slot="chat-input-popup"] [data-index="${popup.selected}"]`,
    )
    if (typeof el?.scrollIntoView === 'function') {
      el.scrollIntoView({ block: 'nearest' })
    }
  }, [popup])

  const submit = () => {
    const text = value.trim()
    if (!text || sendDisabled) return
    onSend(text)
    setValue('')
    setPopup(null)
  }

  /** 接受弹层候选：替换片段起点到文本末尾；完整候选补尾随空格结束标记 */
  const accept = (index: number) => {
    if (!popup) return
    const candidate = popup.candidates[index]
    if (!candidate) return
    setValue(value.slice(0, popup.start) + candidate.value + (candidate.complete ? ' ' : ''))
    setPopup(null)
    textareaRef.current?.focus()
  }

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (popup) {
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        setPopup({ ...popup, selected: (popup.selected + 1) % popup.candidates.length })
        return
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault()
        setPopup({
          ...popup,
          selected: (popup.selected - 1 + popup.candidates.length) % popup.candidates.length,
        })
        return
      }
      if (e.key === 'Tab' || (e.key === 'Enter' && !e.nativeEvent.isComposing)) {
        e.preventDefault()
        accept(popup.selected)
        return
      }
      if (e.key === 'Escape') {
        e.preventDefault()
        setPopup(null)
        return
      }
    }
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault()
      submit()
    }
  }

  return (
    <div className="mx-auto w-full max-w-page px-7 pb-2 pt-1.5">
      {queued > 0 && (
        <div className="mb-2 text-center text-xs text-muted-foreground">
          已排队 {queued} 条，当前轮完成后按序发送
        </div>
      )}
      <div className="relative">
        {/* 补全弹层（`@` mention / `/` 命令） */}
        {popup && (
          <div
            data-slot="chat-input-popup"
            className="absolute bottom-full left-0 mb-1.5 w-full max-w-md overflow-y-auto rounded-lg border bg-popover p-1 shadow-md"
            style={{ maxHeight: 16 * LINE_HEIGHT }}
          >
            {popup.candidates.map((candidate, index) => (
              <button
                key={candidate.value}
                type="button"
                data-index={index}
                // onMouseDown 阻止失焦，保持输入框焦点
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => accept(index)}
                onMouseEnter={() => setPopup({ ...popup, selected: index })}
                className={cn(
                  'flex w-full items-baseline gap-2 rounded-md px-2.5 py-1.5 text-left text-sm',
                  index === popup.selected ? 'bg-accent text-accent-foreground' : '',
                )}
              >
                <span className="shrink-0 font-mono text-xs">{candidate.label}</span>
                {candidate.hint && (
                  <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
                    {candidate.hint}
                  </span>
                )}
              </button>
            ))}
          </div>
        )}

        <div
          className={cn(
            'rounded-xl border bg-card shadow-sm',
            'focus-within:ring-1 focus-within:ring-ring/40',
          )}
        >
          {/* 输入区主体 */}
          <div className="flex items-start gap-2 px-3.5 pt-2">
            {/* 输入框 */}
            <Textarea
              ref={textareaRef}
              value={value}
              onChange={(e) => setValue(e.target.value)}
              onKeyDown={onKeyDown}
              placeholder={placeholder}
              rows={1}
              autoFocus
              style={{ maxHeight: MAX_HEIGHT }}
              className="min-h-7 flex-1 resize-none border-0 bg-transparent px-0 py-0.5 text-base shadow-none focus-visible:ring-0 placeholder:text-muted-foreground/50"
            />
          </div>

          {/* 底部操作栏 */}
          <div className="flex items-center gap-2 px-3.5 pb-2 pt-1">
            <div className="flex-1" />

            {/* 模型选择器 */}
            {modelSpec && onSwitchModel && (
              <ModelPicker
                currentSpec={modelSpec}
                reasoning={reasoning ?? null}
                onSwitch={onSwitchModel}
              />
            )}

            {/* 上下文用量环形指示器 */}
            <ContextRing tokens={contextTokens} window={contextWindow} />

            {/* 发送/停止按钮 */}
            {running ? (
              <Button
                type="button"
                size="icon"
                variant="outline"
                onClick={onStop}
                className="size-7 shrink-0 rounded-full"
                title="停止当前运行（队列保留）"
              >
                <Square className="size-3 fill-current" />
              </Button>
            ) : (
              <Button
                type="button"
                size="icon"
                onClick={submit}
                disabled={!value.trim() || sendDisabled}
                className="size-7 shrink-0 rounded-full"
                title="发送（Enter）"
              >
                <SendHorizontal className="size-3.5" />
              </Button>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
