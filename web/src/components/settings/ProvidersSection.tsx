// providers 设置段：provider 定义列表 + 新建/编辑对话框。
// 补丁语义（与服务端三态对齐）：api / base_url 所见即所得（空 = 清除）；
// api_key 留空 = 不改动（已有值不明文回显），勾选「清除」置 null。

import { useState } from 'react'
import { Pencil, Plus, Trash2 } from 'lucide-react'

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
import type { ApiKind, ProviderPatch, ProviderView } from '@/lib/types'

interface ProvidersSectionProps {
  providers: ProviderView[]
  onSave: (name: string, patch: ProviderPatch) => Promise<unknown>
  onDelete: (name: string) => Promise<unknown>
}

const API_INFER = '__infer__'

export function ProvidersSection({ providers, onSave, onDelete }: ProvidersSectionProps) {
  const [editing, setEditing] = useState<ProviderView | 'new' | null>(null)
  const [error, setError] = useState<string | null>(null)

  const handleDelete = async (name: string) => {
    if (!window.confirm(`删除 provider「${name}」？其模型覆盖将一并清除。`)) return
    try {
      await onDelete(name)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  return (
    <section className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h2 className="text-body font-medium">Providers</h2>
        <Button variant="outline" size="sm" onClick={() => setEditing('new')}>
          <Plus />
          添加 provider
        </Button>
      </div>
      {error && (
        <p role="alert" className="text-body-sm text-destructive">
          {error}
        </p>
      )}
      {providers.length === 0 ? (
        <p className="text-body-sm text-muted-foreground">还没有 provider 定义。</p>
      ) : (
        <ul role="list" className="flex flex-col gap-2">
          {providers.map((provider) => (
            <li
              key={provider.name}
              className="flex items-center justify-between gap-4 rounded-lg border px-4 py-3"
            >
              <span className="min-w-0 truncate text-body-sm font-medium">{provider.name}</span>
              <div className="flex shrink-0 gap-1">
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => setEditing(provider)}
                  aria-label={`编辑 ${provider.name}`}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => void handleDelete(provider.name)}
                  aria-label={`删除 ${provider.name}`}
                >
                  <Trash2 />
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}
      {editing !== null && (
        <ProviderDialog
          provider={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
          onSave={async (name, patch) => {
            await onSave(name, patch)
            setEditing(null)
          }}
        />
      )}
    </section>
  )
}

function ProviderDialog({
  provider,
  onClose,
  onSave,
}: {
  provider: ProviderView | null
  onClose: () => void
  onSave: (name: string, patch: ProviderPatch) => Promise<void>
}) {
  const [name, setName] = useState(provider?.name ?? '')
  const [api, setApi] = useState<string>(provider?.api ?? API_INFER)
  const [baseUrl, setBaseUrl] = useState(provider?.base_url ?? '')
  const [apiKey, setApiKey] = useState('')
  const [clearApiKey, setClearApiKey] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)

  const handleSave = async () => {
    const patch: ProviderPatch = {
      api: api === API_INFER ? null : (api as ApiKind),
      base_url: baseUrl.trim() === '' ? null : baseUrl.trim(),
    }
    if (clearApiKey) patch.api_key = null
    else if (apiKey !== '') patch.api_key = apiKey
    setSaving(true)
    setError(null)
    try {
      await onSave(name.trim(), patch)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setSaving(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{provider ? `编辑 ${provider.name}` : '添加 provider'}</DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <div className="flex flex-col gap-2">
            <Label htmlFor="provider-name">名称</Label>
            <Input
              id="provider-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              disabled={provider !== null}
              placeholder="如 anthropic、openai、deepseek"
            />
          </div>
          <div className="flex flex-col gap-2">
            <Label>API 种类</Label>
            <Select value={api} onValueChange={setApi}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={API_INFER}>按名推断</SelectItem>
                <SelectItem value="anthropic_messages">anthropic_messages</SelectItem>
                <SelectItem value="open_ai_completions">open_ai_completions</SelectItem>
              </SelectContent>
            </Select>
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="provider-base-url">base_url</Label>
            <Input
              id="provider-base-url"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder="https://…"
            />
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="provider-api-key">api_key</Label>
            <Input
              id="provider-api-key"
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              disabled={clearApiKey}
              placeholder={provider?.has_api_key ? '已设置' : 'sk-…'}
            />
            {provider?.has_api_key && (
              <label className="flex items-center gap-2 text-body-sm text-muted-foreground">
                <input
                  type="checkbox"
                  checked={clearApiKey}
                  onChange={(e) => setClearApiKey(e.target.checked)}
                />
                清除已设置的 api_key
              </label>
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
          <Button onClick={() => void handleSave()} disabled={saving || name.trim() === ''}>
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
