// 标量设置段：简单取值（布尔 / 数字 / 普通字符串）行内直接编辑——布尔用开关
// 即点即存，数字与字符串在输入框内 Enter/失焦提交、Esc 还原；密钥与 JSON 等
// 复杂取值仍走编辑对话框。api_key 服务端已脱敏为「已设置」标记，编辑时输入新值覆盖。
// 布局：无边框平铺列表 + 细分隔线，编辑按钮悬停行时才显现。

import { useState } from 'react'
import { Pencil } from 'lucide-react'

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
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { cn } from '@/lib/utils'

interface ScalarsSectionProps {
  values: Record<string, unknown>
  keys: string[]
  onSet: (key: string, value: unknown) => Promise<unknown>
}

type ScalarKind = 'string' | 'number' | 'boolean' | 'json'

const KEY_META: Record<string, { label: string; kind: ScalarKind; secret?: boolean }> = {
  base_url: { label: '全局 base_url', kind: 'string' },
  api_key: { label: '全局 api_key', kind: 'string', secret: true },
  temperature: { label: '采样温度', kind: 'number' },
  max_tokens: { label: '最大输出 token 数', kind: 'number' },
  append_system: { label: '追加系统提示词', kind: 'string' },
  prompts: { label: '额外 prompt template 路径', kind: 'json' },
  'compaction.enabled': { label: '自动压缩开关', kind: 'boolean' },
  'compaction.reserve_tokens': { label: '压缩预留 tokens', kind: 'number' },
  'compaction.keep_recent_tokens': { label: '压缩保留近期 tokens', kind: 'number' },
  model_aliases: { label: '模型别名表', kind: 'json' },
}

export function ScalarsSection({ values, keys, onSet }: ScalarsSectionProps) {
  // 行内输入框与对话框各自就地回显错误；段落级 alert 仅服务开关。
  const [editing, setEditing] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  const report = (e: unknown) => setError(e instanceof Error ? e.message : String(e))

  return (
    <section className="flex flex-col gap-4">
      <h2 className="text-body font-medium">标量设置</h2>
      {error && (
        <p role="alert" className="text-body-sm text-destructive">
          {error}
        </p>
      )}
      <ul role="list" className="flex flex-col divide-y divide-border">
        {keys.map((key) => {
          const meta = KEY_META[key]
          const kind: ScalarKind = meta?.kind ?? 'string'
          const set = key in values
          const needsDialog = kind === 'json' || meta?.secret === true
          return (
            <li key={key} className="group flex items-center justify-between gap-4 py-3">
              <div className="flex min-w-0 flex-col gap-0.5">
                <span className="truncate text-body-sm font-medium">
                  {meta?.label ?? key}
                </span>
                <code className="truncate text-caption text-muted-foreground">{key}</code>
              </div>
              <div className="flex shrink-0 items-center gap-2">
                {kind === 'boolean' ? (
                  <Switch
                    checked={values[key] === true}
                    onCheckedChange={(checked) => {
                      setError(null)
                      void onSet(key, checked).catch(report)
                    }}
                    aria-label={key}
                  />
                ) : needsDialog ? (
                  <>
                    <span className="max-w-48 truncate text-body-sm text-muted-foreground">
                      {set ? (meta?.secret ? '已设置' : '已设置（JSON）') : '未设置'}
                    </span>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      onClick={() => setEditing(key)}
                      aria-label={`编辑 ${key}`}
                      className="opacity-0 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100"
                    >
                      <Pencil />
                    </Button>
                  </>
                ) : (
                  <InlineScalarInput
                    scalarKey={key}
                    kind={kind}
                    current={values[key]}
                    onSet={onSet}
                  />
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

// 行内输入框：本地草稿态，Enter/失焦提交，Esc 还原；提交失败回显错误并还原为当前值。
function InlineScalarInput({
  scalarKey,
  kind,
  current,
  onSet,
}: {
  scalarKey: string
  kind: 'string' | 'number'
  current: unknown
  onSet: (key: string, value: unknown) => Promise<unknown>
}) {
  const committed = current === undefined ? '' : String(current)
  const [text, setText] = useState(committed)
  const [dirty, setDirty] = useState(false)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [lastCommitted, setLastCommitted] = useState(committed)

  // 快照重拉后同步外部值（渲染期间派生）；编辑中（dirty）不打断用户输入。
  if (committed !== lastCommitted) {
    setLastCommitted(committed)
    if (!dirty) setText(committed)
  }

  const revert = () => {
    setText(committed)
    setDirty(false)
  }

  const commit = async () => {
    const raw = text.trim()
    if (raw === committed) {
      revert()
      return
    }
    if (raw === '') {
      // 置空语义不被接受：留空即还原。
      revert()
      return
    }
    let value: unknown = raw
    if (kind === 'number') {
      value = Number(raw)
      if (!Number.isFinite(value)) {
        setError(`应为数字：${raw}`)
        revert()
        return
      }
    }
    setSaving(true)
    setError(null)
    try {
      await onSet(scalarKey, value)
      setDirty(false)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      revert()
    } finally {
      setSaving(false)
    }
  }

  return (
    <span className="flex items-center gap-2">
      {error && (
        <span role="alert" className="text-body-sm text-destructive">
          {error}
        </span>
      )}
      <Input
        aria-label={scalarKey}
        type="text"
        inputMode={kind === 'number' ? 'decimal' : undefined}
        value={text}
        disabled={saving}
        placeholder="未设置"
        className={cn('h-8', kind === 'number' ? 'w-32 text-right' : 'w-64')}
        onChange={(e) => {
          setText(e.target.value)
          setDirty(true)
        }}
        onBlur={() => void commit()}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.currentTarget.blur()
          } else if (e.key === 'Escape') {
            revert()
          }
        }}
      />
    </span>
  )
}

// 对话框仅服务复杂取值：JSON（多行）与密钥（脱敏，不明文回显）。
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
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)

  const handleSave = async () => {
    let value: unknown
    if (kind === 'json') {
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
            {kind === 'json' ? (
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
                type={isSecret ? 'password' : 'text'}
                value={text}
                onChange={(e) => setText(e.target.value)}
                placeholder={isSecret ? '已设置' : undefined}
              />
            )}
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
          <Button onClick={() => void handleSave()} disabled={saving || text.trim() === ''}>
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
