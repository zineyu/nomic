// 标量设置段：仅暴露适合 web 端调整的键（追加系统提示词 / prompt template 路径 /
// 自动压缩开关 / 模型别名表）；base_url、api_key、temperature、max_tokens 与
// compaction 的 token 参数不在页面展示，一律使用默认值（或经 CLI `nomic config`
// 配置），即使快照下发了这些键也会被过滤。
// 简单取值行内直接编辑——布尔开关即点即存，字符串输入框 Enter/失焦提交、Esc 还原；
// JSON 取值走编辑对话框。布局：无边框平铺列表 + 细分隔线，编辑按钮悬停行时才显现。

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

interface ScalarsSectionProps {
  values: Record<string, unknown>
  keys: string[]
  onSet: (key: string, value: unknown) => Promise<unknown>
}

type ScalarKind = 'string' | 'boolean' | 'json'

const KEY_META: Record<string, { label: string; kind: ScalarKind }> = {
  append_system: { label: '追加系统提示词', kind: 'string' },
  prompts: { label: '额外 prompt template 路径', kind: 'json' },
  'compaction.enabled': { label: '自动压缩开关', kind: 'boolean' },
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
          if (!meta) return null
          const set = key in values
          return (
            <li key={key} className="group flex items-center justify-between gap-4 py-3">
              <div className="flex min-w-0 flex-col gap-0.5">
                <span className="truncate text-body-sm font-medium">{meta.label}</span>
                <code className="truncate text-caption text-muted-foreground">{key}</code>
              </div>
              <div className="flex shrink-0 items-center gap-2">
                {meta.kind === 'boolean' ? (
                  <Switch
                    checked={values[key] === true}
                    onCheckedChange={(checked) => {
                      setError(null)
                      void onSet(key, checked).catch(report)
                    }}
                    aria-label={key}
                  />
                ) : meta.kind === 'json' ? (
                  <>
                    <span className="max-w-48 truncate text-body-sm text-muted-foreground">
                      {set ? '已设置' : '未设置'}
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
                  <InlineStringInput scalarKey={key} current={values[key]} onSet={onSet} />
                )}
              </div>
            </li>
          )
        })}
      </ul>
      {editing !== null && (
        <JsonDialog
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
function InlineStringInput({
  scalarKey,
  current,
  onSet,
}: {
  scalarKey: string
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
    if (raw === committed || raw === '') {
      // 置空语义不被接受：留空即还原，这些键全部回退默认值。
      revert()
      return
    }
    setSaving(true)
    setError(null)
    try {
      await onSet(scalarKey, raw)
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
        value={text}
        disabled={saving}
        placeholder="未设置"
        className="h-8 w-64"
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

// JSON 取值（prompt 模板路径 / 模型别名表）的编辑对话框：多行文本，保存前解析校验。
function JsonDialog({
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
  const [text, setText] = useState(() =>
    current === undefined ? '' : JSON.stringify(current, null, 2),
  )
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)

  const handleSave = async () => {
    let value: unknown
    try {
      value = JSON.parse(text)
    } catch {
      setError('JSON 解析失败')
      return
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
            <Textarea
              id="scalar-value"
              value={text}
              onChange={(e) => setText(e.target.value)}
              rows={4}
              className="font-mono"
            />
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
