// 模型选择器：跨 provider 候选列表（dropdown-menu 分组）；推理模型附
// 思考级别选择。切换复用服务端同一分层口径并落库（与 TUI /models 一致）。
// 输入区圆角胶囊形态：收缩到内容宽度，不抢占输入框剩余空间。

import { useEffect, useMemo, useState } from 'react'
import { Check, ChevronsUpDown, Cpu, Brain } from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { api } from '@/lib/api'
import type { ModelChoice, ModelsResponse } from '@/lib/types'
import { cn } from '@/lib/utils'

const LEVELS = ['off', 'minimal', 'low', 'medium', 'high'] as const

interface ModelPickerProps {
  currentSpec: string | null
  reasoning: string | null
  onSwitch: (spec: string, reasoning?: string) => void
}

export function ModelPicker({
  currentSpec,
  reasoning,
  onSwitch,
}: ModelPickerProps) {
  const [data, setData] = useState<ModelsResponse | null>(null)
  const [open, setOpen] = useState(false)
  const [levelOpen, setLevelOpen] = useState(false)

  useEffect(() => {
    void api.models().then(setData).catch(() => {})
  }, [open, levelOpen])

  const groups = useMemo(() => {
    if (!data) return new Map<string, ModelChoice[]>()
    const map = new Map<string, ModelChoice[]>()
    for (const choice of data.candidates) {
      const list = map.get(choice.provider) ?? []
      list.push(choice)
      map.set(choice.provider, list)
    }
    return map
  }, [data])

  const currentModel = data?.candidates.find(
    (c) => `${c.provider}/${c.id}` === currentSpec,
  )
  const currentSupportsReasoning = currentModel?.reasoning ?? false

  const switchTo = async (spec: string) => {
    setOpen(false)
    onSwitch(spec)
  }

  const setLevel = async (level: string) => {
    setLevelOpen(false)
    if (!currentSpec) return
    onSwitch(currentSpec, level)
  }

  return (
    <div className="flex shrink-0 items-center gap-1">
      <DropdownMenu open={open} onOpenChange={setOpen}>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            className="h-7 min-w-0 shrink-0 justify-start gap-1.5 rounded-full border border-border bg-background px-3 text-xs font-normal hover:bg-accent"
          >
            <Cpu className="size-3 shrink-0 opacity-70" />
            <span className="min-w-0 flex-1 truncate text-left">
              {currentSpec ?? '选择模型'}
            </span>
            <ChevronsUpDown className="size-3 shrink-0 opacity-60" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="max-h-96 w-72 overflow-y-auto">
          {[...groups.entries()].map(([provider, choices], i) => (
            <div key={provider}>
              {i > 0 && <DropdownMenuSeparator />}
              <DropdownMenuLabel className="text-xs text-muted-foreground">
                {provider}
              </DropdownMenuLabel>
              {choices.map((choice) => (
                <DropdownMenuItem
                  key={choice.id}
                  onSelect={() => void switchTo(`${choice.provider}/${choice.id}`)}
                  className="flex items-center justify-between gap-2"
                >
                  <span className="truncate">{choice.name}</span>
                  <span className="flex shrink-0 items-center gap-1">
                    {choice.reasoning && <Cpu className="size-3 text-muted-foreground" />}
                    {`${choice.provider}/${choice.id}` === currentSpec && (
                      <Check className="size-3.5 text-primary" />
                    )}
                  </span>
                </DropdownMenuItem>
              ))}
            </div>
          ))}
          {data?.candidates.length === 0 && (
            <div className="px-3 py-2 text-xs text-muted-foreground">
              没有可用模型（检查 config.toml 的 [providers]）
            </div>
          )}
        </DropdownMenuContent>
      </DropdownMenu>

      {currentSupportsReasoning && (
        <DropdownMenu open={levelOpen} onOpenChange={setLevelOpen}>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              className={cn(
                'h-7 shrink-0 gap-1.5 rounded-full px-2 text-xs font-normal',
                reasoning && reasoning !== 'off'
                  ? 'border border-primary/30 bg-primary/10 text-primary'
                  : 'text-muted-foreground',
              )}
              title={reasoning && reasoning !== 'off' ? `推理强度: ${reasoning}` : '推理强度: 关闭'}
            >
              <Brain className="size-3 shrink-0" />
              <span>{reasoning && reasoning !== 'off' ? reasoning : 'off'}</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            {LEVELS.map((level) => (
              <DropdownMenuItem
                key={level}
                onSelect={() => void setLevel(level)}
                className={cn('justify-between gap-4', reasoning === level && 'font-medium')}
              >
                <span className="flex items-center gap-2">
                  <Brain className="size-3" />
                  {level}
                </span>
                {reasoning === level && <Check className="size-3.5 text-primary" />}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </div>
  )
}
