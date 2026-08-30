// 模型规格覆盖设置段：覆盖行列表 + 新建/编辑对话框。
// 对话框所见即所得：字段留空 = 未覆盖（null，向下回退 models.dev / 中性兜底）。

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
import type { ModelSpecPatch, ModelSpecRow, ProviderView } from '@/lib/types'

interface ModelSpecsSectionProps {
  specs: ModelSpecRow[]
  providers: ProviderView[]
  onSave: (provider: string, modelId: string, patch: ModelSpecPatch) => Promise<unknown>
  onDelete: (provider: string, modelId: string) => Promise<unknown>
}

const UNSET = '__unset__'

export function ModelSpecsSection({ specs, providers, onSave, onDelete }: ModelSpecsSectionProps) {
  const [editing, setEditing] = useState<ModelSpecRow | 'new' | null>(null)
  const [error, setError] = useState<string | null>(null)

  return (
    <section className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h2 className="text-body font-medium">模型规格覆盖</h2>
        <Button
          variant="outline"
          size="sm"
          onClick={() => setEditing('new')}
          disabled={providers.length === 0}
          title={providers.length === 0 ? '先添加 provider' : undefined}
        >
          <Plus />
          添加覆盖
        </Button>
      </div>
      {error && (
        <p role="alert" className="text-body-sm text-destructive">
          {error}
        </p>
      )}
      {specs.length === 0 ? (
        <p className="text-body-sm text-muted-foreground">
          没有规格覆盖：规格按 models.dev 目录 &gt; 中性兜底解析；models.dev 缺失或需修正时在此覆盖。
        </p>
      ) : (
        <ul role="list" className="flex flex-col gap-2">
          {specs.map((spec) => (
            <li
              key={`${spec.provider}/${spec.model_id}`}
              className="flex items-center justify-between gap-4 rounded-lg border px-4 py-3"
            >
              <div className="flex min-w-0 flex-col gap-0.5">
                <span className="text-body-sm font-medium">
                  {spec.provider}/{spec.model_id}
                </span>
                <span className="text-caption text-muted-foreground">{summarize(spec)}</span>
              </div>
              <div className="flex shrink-0 gap-1">
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => setEditing(spec)}
                  aria-label={`编辑 ${spec.provider}/${spec.model_id}`}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  onClick={() => {
                    void onDelete(spec.provider, spec.model_id).catch((e: unknown) =>
                      setError(e instanceof Error ? e.message : String(e)),
                    )
                  }}
                  aria-label={`删除 ${spec.provider}/${spec.model_id}`}
                >
                  <Trash2 />
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}
      {editing !== null && (
        <SpecDialog
          spec={editing === 'new' ? null : editing}
          providers={providers}
          onClose={() => setEditing(null)}
          onSave={async (provider, modelId, patch) => {
            await onSave(provider, modelId, patch)
            setEditing(null)
          }}
        />
      )}
    </section>
  )
}

/** 单行摘要：已覆盖字段的紧凑列表。 */
function summarize(spec: ModelSpecRow): string {
  const parts: string[] = []
  if (spec.name !== null) parts.push(`name=${spec.name}`)
  if (spec.reasoning !== null) parts.push(`reasoning=${spec.reasoning}`)
  if (spec.vision !== null) parts.push(`vision=${spec.vision}`)
  if (spec.context_window !== null) parts.push(`context=${spec.context_window}`)
  if (spec.max_tokens !== null) parts.push(`max_tokens=${spec.max_tokens}`)
  if (spec.cost_input !== null) parts.push(`cost_in=${spec.cost_input}`)
  if (spec.cost_output !== null) parts.push(`cost_out=${spec.cost_output}`)
  if (spec.cost_cache_read !== null) parts.push(`cache_read=${spec.cost_cache_read}`)
  if (spec.cost_cache_write !== null) parts.push(`cache_write=${spec.cost_cache_write}`)
  return parts.length === 0 ? '（空覆盖行）' : parts.join(' · ')
}

type BoolField = 'reasoning' | 'vision'
type NumField =
  | 'context_window'
  | 'max_tokens'
  | 'cost_input'
  | 'cost_output'
  | 'cost_cache_read'
  | 'cost_cache_write'

const NUM_FIELDS: { field: NumField; label: string }[] = [
  { field: 'context_window', label: '上下文窗口（tokens）' },
  { field: 'max_tokens', label: '最大输出（tokens）' },
  { field: 'cost_input', label: '费率：输入（$/M）' },
  { field: 'cost_output', label: '费率：输出（$/M）' },
  { field: 'cost_cache_read', label: '费率：缓存读（$/M）' },
  { field: 'cost_cache_write', label: '费率：缓存写（$/M）' },
]

function SpecDialog({
  spec,
  providers,
  onClose,
  onSave,
}: {
  spec: ModelSpecRow | null
  providers: ProviderView[]
  onClose: () => void
  onSave: (provider: string, modelId: string, patch: ModelSpecPatch) => Promise<void>
}) {
  const [provider, setProvider] = useState(spec?.provider ?? providers[0]?.name ?? '')
  const [modelId, setModelId] = useState(spec?.model_id ?? '')
  const [name, setName] = useState(spec?.name ?? '')
  const [bools, setBools] = useState<Record<BoolField, string>>({
    reasoning: spec?.reasoning === null || spec === null ? UNSET : String(spec.reasoning),
    vision: spec?.vision === null || spec === null ? UNSET : String(spec.vision),
  })
  const [nums, setNums] = useState<Record<NumField, string>>({
    context_window: spec?.context_window?.toString() ?? '',
    max_tokens: spec?.max_tokens?.toString() ?? '',
    cost_input: spec?.cost_input?.toString() ?? '',
    cost_output: spec?.cost_output?.toString() ?? '',
    cost_cache_read: spec?.cost_cache_read?.toString() ?? '',
    cost_cache_write: spec?.cost_cache_write?.toString() ?? '',
  })
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)

  const handleSave = async () => {
    const patch: ModelSpecPatch = {
      name: name.trim() === '' ? null : name.trim(),
      reasoning: bools.reasoning === UNSET ? null : bools.reasoning === 'true',
      vision: bools.vision === UNSET ? null : bools.vision === 'true',
    }
    for (const { field, label } of NUM_FIELDS) {
      const raw = nums[field].trim()
      if (raw === '') {
        patch[field] = null
        continue
      }
      const value = Number(raw)
      if (!Number.isFinite(value) || value < 0) {
        setError(`${label} 取值非法：${raw}`)
        return
      }
      patch[field] = value
    }
    setSaving(true)
    setError(null)
    try {
      await onSave(provider, modelId.trim(), patch)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setSaving(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[85vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {spec ? `编辑 ${spec.provider}/${spec.model_id}` : '添加模型规格覆盖'}
          </DialogTitle>
        </DialogHeader>
        <div className="flex flex-col gap-4">
          <div className="grid grid-cols-2 gap-4">
            <div className="flex flex-col gap-2">
              <Label>provider</Label>
              <Select value={provider} onValueChange={setProvider} disabled={spec !== null}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {providers.map((p) => (
                    <SelectItem key={p.name} value={p.name}>
                      {p.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="spec-model-id">模型 id</Label>
              <Input
                id="spec-model-id"
                value={modelId}
                onChange={(e) => setModelId(e.target.value)}
                disabled={spec !== null}
              />
            </div>
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="spec-name">展示名（留空 = 未覆盖）</Label>
            <Input id="spec-name" value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <div className="grid grid-cols-2 gap-4">
            {(['reasoning', 'vision'] as BoolField[]).map((field) => (
              <div key={field} className="flex flex-col gap-2">
                <Label>{field === 'reasoning' ? '支持推理' : '支持图像输入'}</Label>
                <Select
                  value={bools[field]}
                  onValueChange={(v) => setBools((prev) => ({ ...prev, [field]: v }))}
                >
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={UNSET}>未覆盖</SelectItem>
                    <SelectItem value="true">是</SelectItem>
                    <SelectItem value="false">否</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            ))}
          </div>
          <div className="grid grid-cols-2 gap-4">
            {NUM_FIELDS.map(({ field, label }) => (
              <div key={field} className="flex flex-col gap-2">
                <Label htmlFor={`spec-${field}`}>{label}</Label>
                <Input
                  id={`spec-${field}`}
                  inputMode="decimal"
                  value={nums[field]}
                  onChange={(e) => setNums((prev) => ({ ...prev, [field]: e.target.value }))}
                  placeholder="未覆盖"
                />
              </div>
            ))}
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
            disabled={saving || provider === '' || modelId.trim() === ''}
          >
            保存
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
