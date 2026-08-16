// 模型选择器：跨 provider 候选列表（dropdown-menu 分组）；推理模型附
// 思考级别选择。切换复用服务端同一分层口径并落库（与 TUI /models 一致）。

import { useEffect, useMemo, useState } from 'react'
import { Check, ChevronsUpDown, Cpu } from 'lucide-react'

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
  onChanged: () => void
}

export function ModelPicker({ currentSpec, reasoning, onChanged }: ModelPickerProps) {
  const [data, setData] = useState<ModelsResponse | null>(null)
  const [open, setOpen] = useState(false)
  const [levelOpen, setLevelOpen] = useState(false)

  useEffect(() => {
    void api.models().then(setData).catch(() => {})
  }, [open])

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
    try {
      await api.switchModel(spec)
      onChanged()
    } catch (error) {
      console.error('切换模型失败', error)
    }
  }

  const setLevel = async (level: string) => {
    setLevelOpen(false)
    if (!currentSpec) return
    try {
      await api.switchModel(currentSpec, level)
      onChanged()
    } catch (error) {
      console.error('设置思考级别失败', error)
    }
  }

  return (
    <div className="flex items-center gap-1">
      <DropdownMenu open={open} onOpenChange={setOpen}>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="sm" className="h-7 gap-1 px-2 text-xs font-normal">
            <Cpu className="size-3.5" />
            <span className="max-w-40 truncate">
              {currentSpec ?? '选择模型'}
            </span>
            <ChevronsUpDown className="size-3 opacity-60" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="max-h-96 w-72 overflow-y-auto">
          {[...groups.entries()].map(([provider, choices], i) => (
            <div key={provider}>
              {i > 0 && <DropdownMenuSeparator />}
              <DropdownMenuLabel className="text-[11px] text-muted-foreground">
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
            <Button variant="ghost" size="sm" className="h-7 px-2 text-[11px] font-normal text-muted-foreground">
              {reasoning ?? 'off'}
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start">
            {LEVELS.map((level) => (
              <DropdownMenuItem
                key={level}
                onSelect={() => void setLevel(level)}
                className={cn('justify-between gap-4', reasoning === level && 'font-medium')}
              >
                {level}
                {reasoning === level && <Check className="size-3.5 text-primary" />}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </div>
  )
}
