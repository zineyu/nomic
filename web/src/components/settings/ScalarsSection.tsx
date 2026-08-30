// 标量设置段：已知键的当前值列表 + 编辑/清除。
// 编辑器按键的取值类型分派（数字 / 布尔 / 字符串 / JSON）；api_key 服务端
// 已脱敏为「已设置」标记，编辑时输入新值覆盖。

import { useState } from 'react'
import { Pencil, Trash2 } from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Textarea } from '@/components/ui/textarea'

interface ScalarsSectionProps {
  values: Record<string, unknown>
  keys: string[]
  onSet: (key: string, value: unknown) => Promise<unknown>
  onUnset: (key: string) => Promise<unknown>
}

type ScalarKind = 'string' | 'number' | 'boolean' | 'json'

const KEY_META: Record<string, { label: string; kind: ScalarKind; secret?: boolean; hint?: string }> = {
  base_url: { label: '全局 base_url 兜底', kind: 'string' },
  api_key: { label: '全局 api_key 兜底', kind: 'string', secret: true, hint: '建议优先用环境变量' },
  temperature: { label: '采样温度', kind: 'number' },
  max_tokens: { label: '最大输出 token 数', kind: 'number' },
  append_system: { label: '追加系统提示词', kind: 'string' },
  prompts: { label: '额外 prompt template 路径', kind: 'json', hint: 'JSON 数组，如 ["prompts/review.md"]' },
  'compaction.enabled': { label: '自动压缩开关', kind: 'boolean' },
  'compaction.reserve_tokens': { label: '压缩预留 tokens', kind: 'number' },
  'compaction.keep_recent_tokens': { label: '压缩保留近期 tokens', kind: 'number' },
  model_aliases: { label: '模型别名表', kind: 'json', hint: 'JSON 对象，如 {"smart":"openai/gpt-4o"}' },
}

export function ScalarsSection({ values, keys, onSet, onUnset }: ScalarsSectionProps) {
  const [editing, setEditing] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  return (
    <section className="flex flex-col gap-4">
      <h2 className="text-body font-medium">标量设置</h2>
      {error && (
        <p role="alert" className="text-body-sm text-destructive">
          {error}
        </p>
      )}
      <ul role="list" className="flex flex-col gap-2">
        {keys.map((key) => {
          const meta = KEY_META[key]
          const set = key in values
          return (
            <li
              key={key}
              className="flex items-center justify-between gap-4 rounded-lg border px-4 py-3"
            >
              <div className="flex min-w-0 flex-col gap-0.5">
                <span className="text-body-sm font-medium">{meta?.label ?? key}</span>
                <span className="text-caption text-muted-foreground">
                  <code>{key}</code>
                  {' · '}
                  <span>{set ? formatValue(key, values[key]) : '未设置'}</span>
                </span>
              </div>
              <div className="flex shrink-0 gap-1">
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => setEditing(key)}
                  aria-label={`编辑 ${key}`}
                >
                  <Pencil />
                </Button>
                {set && (
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    onClick={() => {
                      void onUnset(key).catch((e: unknown) =>
                        setError(e instanceof Error ? e.message : String(e)),
                      )
                    }}
                    aria-label={`清除 ${key}`}
                  >
                    <Trash2 />
                  </Button>
                )}
              </div>
            </li>
          )
        })}
      </ul>
      {editing !== null && (
        <ScalarDialog
          scalarKey={editing}
          current={values[editing]}
          onClose={() => setEditing(null)}
          onSet={async (key, value) => {
            await onSet(key, value)
            setEditing(null)
          }}
        />
      )}
    </section>
  )
}

/** 当前值展示：字符串原样（secret 已由服务端脱敏），其余 JSON。 */
function formatValue(key: string, value: unknown): string {
  if (KEY_META[key]?.secret) return '已设置'
  if (typeof value === 'string') return value
  return JSON.stringify(value)
}

function ScalarDialog({
  scalarKey,
  current,
  onClose,
  onSet,
}: {
  scalarKey: string
  current: unknown
  onClose: () => void
  onSet: (key: string, value: unknown) => Promise<void>
}) {
  const meta = KEY_META[scalarKey]
  const kind: ScalarKind = meta?.kind ?? 'string'
  const isSecret = meta?.secret === true
  const [text, setText] = useState(() => {
    if (current === undefined || isSecret) return ''
    if (kind === 'json') return JSON.stringify(current, null, 2)
    return String(current)
  })
  const [boolValue, setBoolValue] = useState(String(current === true))
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)

  const handleSave = async () => {
    let value: unknown
    if (kind === 'boolean') {
      value = boolValue === 'true'
    } else if (kind === 'number') {
      value = Number(text.trim())
      if (!Number.isFinite(value)) {
        setError(`应为数字：${text}`)
        return
      }
    } else if (kind === 'json') {
      try {
        value = JSON.parse(text)
      } catch {
        setError('JSON 解析失败')
        return
      }
    } else {
      value = text
    }
    setSaving(true)
    setError(null)
    try {
      await onSet(scalarKey, value)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setSaving(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{meta?.label ?? scalarKey}</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <div className="flex flex-col gap-2">
            <Label htmlFor="scalar-value">
              <code className="text-caption">{scalarKey}</code>
            </Label>
            {kind === 'boolean' ? (
              <Select value={boolValue} onValueChange={setBoolValue}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="true">开启</SelectItem>
                  <SelectItem value="false">关闭</SelectItem>
                </SelectContent>
              </Select>
            ) : kind === 'json' ? (
              <Textarea
                id="scalar-value"
                value={text}
                onChange={(e) => setText(e.target.value)}
                rows={4}
                className="font-mono"
              />
            ) : (
              <Input
                id="scalar-value"
                type={isSecret ? 'password' : kind === 'number' ? 'number' : 'text'}
                value={text}
                onChange={(e) => setText(e.target.value)}
                placeholder={isSecret ? '已设置（输入新值覆盖）' : undefined}
              />
            )}
            {meta?.hint && <p className="text-caption text-muted-foreground">{meta.hint}</p>}
          </div>
          {error && (
            <p role="alert" className="text-body-sm text-destructive">
              {error}
            </p>
          )}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            取消
          </Button>
          <Button
            onClick={() => void handleSave()}
            disabled={saving || (kind !== 'boolean' && text.trim() === '')}
          >
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
