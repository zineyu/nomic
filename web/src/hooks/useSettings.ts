// useSettings：设置页状态入口——`get_settings` 快照为唯一数据源，写操作经
// WS 查询式命令（ack / error 关联 request_id），成功后由服务端广播的
// `settings_changed` 驱动所有客户端重拉（本页不再本地改状态）。
// `active` 为 false（设置页未展示）时不连接不订阅。

import { useCallback, useEffect, useState } from 'react'

import { api } from '@/lib/api'
import type { ModelSpecPatch, ProviderPatch, SettingsSnapshot } from '@/lib/types'

export interface SettingsState {
  /** 设置快照（null = 尚未加载） */
  snapshot: SettingsSnapshot | null
  /** 最近一次拉取失败信息（写操作错误由各对话框自行捕获展示） */
  error: string | null
  refresh: () => Promise<void>
  upsertProvider: (name: string, patch: ProviderPatch) => Promise<unknown>
  deleteProvider: (name: string) => Promise<unknown>
  upsertModelSpec: (provider: string, modelId: string, patch: ModelSpecPatch) => Promise<unknown>
  deleteModelSpec: (provider: string, modelId: string) => Promise<unknown>
  setSetting: (key: string, value: unknown) => Promise<unknown>
  unsetSetting: (key: string) => Promise<unknown>
}

export function useSettings(active: boolean): SettingsState {
  const [snapshot, setSnapshot] = useState<SettingsSnapshot | null>(null)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    try {
      setSnapshot(await api.settings())
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])

  useEffect(() => {
    if (!active) return
    void api.connect().then(refresh)
    // 写操作完成后服务端广播 settings_changed；断线重连发 refresh，两者都重拉
    return api.subscribe((event) => {
      if (event.type === 'settings_changed' || event.type === 'refresh') void refresh()
    })
  }, [active, refresh])

  return {
    snapshot,
    error,
    refresh,
    upsertProvider: api.upsertProvider,
    deleteProvider: api.deleteProvider,
    upsertModelSpec: api.upsertModelSpec,
    deleteModelSpec: api.deleteModelSpec,
    setSetting: api.setSetting,
    unsetSetting: api.unsetSetting,
  }
}
