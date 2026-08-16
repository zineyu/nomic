// ask_user_question 提问弹层：单选 / 多选 / 填空；单选多选自动带
// 「自定义填写」选项（末位），选中后展开文本输入。回答经 REST 回填服务端。

import { useState } from 'react'

import { Check, PencilLine } from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'
import type { AskUserAnswer, AskUserQuestion } from '@/lib/types'

const CUSTOM_OPTION = '✏️ 其他（自定义填写）'

interface QuestionModalProps {
  id: string
  question: AskUserQuestion
  onAnswer: (id: string, answer: AskUserAnswer) => void
}

export function QuestionModal({ id, question, onAnswer }: QuestionModalProps) {
  const [selected, setSelected] = useState<string[]>([])
  const [custom, setCustom] = useState('')
  const [fillIn, setFillIn] = useState('')

  const isFillIn = question.kind === 'fill_in'
  const isSingle = question.kind === 'single_choice'
  const customSelected = selected.includes(CUSTOM_OPTION)

  const toggle = (option: string) => {
    if (isSingle) {
      setSelected(customSelected && option === CUSTOM_OPTION ? [] : [option])
    } else {
      setSelected((prev) =>
        prev.includes(option) ? prev.filter((o) => o !== option) : [...prev, option],
      )
    }
  }

  const canSubmit = isFillIn ? fillIn.trim() !== '' : selected.length > 0

  const submit = () => {
    if (isFillIn) {
      const text = fillIn.trim()
      onAnswer(id, { answers: [text], custom: text })
      return
    }
    const answers = selected.filter((o) => o !== CUSTOM_OPTION)
    const customText = customSelected ? custom.trim() : null
    if (customText) answers.push(customText)
    if (answers.length === 0 && isSingle && customSelected && custom.trim()) {
      answers.push(custom.trim())
    }
    onAnswer(id, { answers, custom: customText })
  }

  return (
    <Dialog open onOpenChange={() => {}}>
      <DialogContent className="sm:max-w-md" showCloseButton={false}>
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2 text-base">
            <PencilLine className="size-4 text-primary" />
            nomic 提问
          </DialogTitle>
        </DialogHeader>
        <div className="text-sm leading-relaxed">{question.question}</div>

        {isFillIn ? (
          <Textarea
            autoFocus
            value={fillIn}
            onChange={(e) => setFillIn(e.target.value)}
            placeholder="输入回答…"
            rows={3}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submit()
            }}
          />
        ) : (
          <div className="max-h-64 space-y-1.5 overflow-y-auto">
            {question.options.map((option) => {
              const checked = selected.includes(option)
              const isCustom = option === CUSTOM_OPTION
              return (
                <div key={option}>
                  <button
                    type="button"
                    onClick={() => toggle(option)}
                    className={cn(
                      'flex w-full items-center gap-2.5 rounded-lg border px-3 py-2 text-left text-sm transition-colors',
                      checked
                        ? 'border-primary bg-primary/10 text-foreground'
                        : 'hover:bg-accent/60',
                    )}
                  >
                    <span
                      className={cn(
                        'flex size-4 shrink-0 items-center justify-center rounded border',
                        isSingle ? 'rounded-full' : 'rounded',
                        checked ? 'border-primary bg-primary' : 'border-input',
                      )}
                    >
                      {checked && <Check className="size-3 text-primary-foreground" />}
                    </span>
                    <span className="min-w-0 flex-1">{option}</span>
                  </button>
                  {isCustom && checked && (
                    <Input
                      autoFocus={isSingle}
                      value={custom}
                      onChange={(e) => setCustom(e.target.value)}
                      placeholder="填写自定义答案…"
                      className="mt-1.5"
                    />
                  )}
                </div>
              )
            })}
          </div>
        )}

        <DialogFooter>
          <Button onClick={submit} disabled={!canSubmit} className="w-full sm:w-auto">
            提交
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
