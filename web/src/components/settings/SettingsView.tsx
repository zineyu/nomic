// 设置页：providers / 模型覆盖 / 标量设置三段（ADR-0039）。
// 数据唯一来源是 useSettings 快照；写操作成功后经 settings_changed 广播重拉。

import { RefreshCw } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useSettings } from '@/hooks/useSettings'

import { ModelSpecsSection } from './ModelSpecsSection'
import { ProvidersSection } from './ProvidersSection'
import { ScalarsSection } from './ScalarsSection'

export function SettingsView() {
  const settings = useSettings(true)

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="max-w-page mx-auto flex w-full flex-col gap-8 px-6 py-8">
        <div className="flex items-center justify-between">
          <h1 className="text-h3">设置</h1>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => void settings.refresh()}
            aria-label="刷新设置"
            title="刷新"
          >
            <RefreshCw />
          </Button>
        </div>

        {settings.error && (
          <p role="alert" className="text-body-sm text-destructive">
            加载设置失败：{settings.error}
          </p>
        )}
        {!settings.error && !settings.snapshot && (
          <div className="flex flex-col gap-4" aria-busy="true" aria-label="加载设置中">
            <Skeleton className="h-24 rounded-lg" />
            <Skeleton className="h-24 rounded-lg" />
            <Skeleton className="h-24 rounded-lg" />
          </div>
        )}
        {settings.snapshot && (
          <>
            <ProvidersSection
              providers={settings.snapshot.providers}
              onSave={settings.upsertProvider}
              onDelete={settings.deleteProvider}
            />
            <ModelSpecsSection
              specs={settings.snapshot.model_specs}
              providers={settings.snapshot.providers}
              onSave={settings.upsertModelSpec}
              onDelete={settings.deleteModelSpec}
            />
            <ScalarsSection
              values={settings.snapshot.settings}
              keys={settings.snapshot.scalar_keys}
              onSet={settings.setSetting}
            />
          </>
        )}
      </div>
    </div>
  )
}
