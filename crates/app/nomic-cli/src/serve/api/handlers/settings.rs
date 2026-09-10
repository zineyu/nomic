//! 设置命令 handler（ADR-0039）：sqlite 设置（providers / model_specs /
//! 标量）的查询与修改，全部经 WebSocket 事件协议（无 REST）。
//!
//! 写操作成功后：刷新进程级模型解析器的设置快照（运行进程立即
//! 生效），ack 携带 `request_id` 返回给发起方，并向总线广播
//! [`ServerEvent::SettingsChanged`]（无 session 维度，同 `Refresh` 先例），
//! 其他客户端据此重新拉取 `get_settings` 快照。

use std::collections::BTreeMap;

use nomic_session::{ModelSpecPatch, ModelSpecRow, ProviderPatch, ProviderRow};
use serde::Serialize;

use super::super::{ApiError, ClientEvent};
use crate::serve::{ServerEvent, WebState};
use crate::settings::{keys, validate_provider_patch, validate_scalar};

/// provider 快照视图（脱敏：api_key 不明文下发，只给是否已设置；
/// 编辑用补丁语义，未触碰的字段保持原值）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderView {
    /// provider 名
    pub name: String,
    /// API 种类；`None` = 按名推断
    pub api: Option<nomic_ai::ApiKind>,
    /// API base URL
    pub base_url: Option<String>,
    /// 是否已设置 api_key
    pub has_api_key: bool,
    /// 最后更新时间（Unix 毫秒）
    pub updated_at: u64,
}

impl From<ProviderRow> for ProviderView {
    fn from(row: ProviderRow) -> Self {
        Self {
            name: row.name,
            api: row.api,
            base_url: row.base_url,
            has_api_key: row.api_key.is_some(),
            updated_at: row.updated_at,
        }
    }
}

/// 设置快照视图（`get_settings` 查询的回复负载）：providers 定义、模型
/// 规格覆盖、标量设置全量 + 可用标量键清单（前端渲染设置表单用）。
#[derive(Debug, Clone, Serialize)]
pub struct SettingsSnapshotView {
    /// 全部 provider 定义（按名排序，api_key 已脱敏）
    pub providers: Vec<ProviderView>,
    /// 全部模型覆盖（按 provider、模型 id 排序）
    pub model_specs: Vec<ModelSpecRow>,
    /// 全部标量设置（键 → JSON 值；api_key 键的值已脱敏为是否已设置）
    pub settings: BTreeMap<String, serde_json::Value>,
    /// 可用标量键（写入校验同一口径）
    pub scalar_keys: &'static [&'static str],
}

/// 取进程级 store（serve 模式 session 与设置共用同一库，正常启动必可用）。
fn store_of(state: &WebState) -> Option<nomic_session::SessionStore> {
    state.inner.services.store().cloned()
}

/// store 不可用时的统一错误响应。
fn store_unavailable(request_id: &str) -> ServerEvent {
    ApiError::StoreUnavailable.to_ws_response(Some(request_id))
}

/// 写后收尾：刷新设置快照 + 广播 `settings_changed` + 返回 ack。
async fn finish_mutation(state: &WebState, request_id: &str) -> ServerEvent {
    let services = &state.inner.services;
    services.models().reload(services.store()).await;
    services.bus().publish(ServerEvent::SettingsChanged);
    ServerEvent::SettingsUpdated {
        request_id: request_id.to_string(),
    }
}

/// 分发设置类客户端事件（主分发表的子表，见 `super::super::dispatch`）。
pub async fn dispatch_settings(state: &WebState, event: ClientEvent) -> ServerEvent {
    match event {
        ClientEvent::GetSettings { request_id } => handle_get_settings(state, &request_id).await,
        ClientEvent::UpsertProvider {
            request_id,
            name,
            patch,
        } => handle_upsert_provider(state, &request_id, &name, patch).await,
        ClientEvent::DeleteProvider { request_id, name } => {
            handle_delete_provider(state, &request_id, &name).await
        }
        ClientEvent::UpsertModelSpec {
            request_id,
            provider,
            model_id,
            patch,
        } => handle_upsert_model_spec(state, &request_id, &provider, &model_id, patch).await,
        ClientEvent::DeleteModelSpec {
            request_id,
            provider,
            model_id,
        } => handle_delete_model_spec(state, &request_id, &provider, &model_id).await,
        ClientEvent::SetSetting {
            request_id,
            key,
            value,
        } => handle_set_setting(state, &request_id, &key, value).await,
        ClientEvent::UnsetSetting { request_id, key } => {
            handle_unset_setting(state, &request_id, &key).await
        }
        other => unreachable!("dispatch_settings 只接设置类事件：{other:?}"),
    }
}

/// 查询设置快照（providers + model_specs + 标量全量）。
pub async fn handle_get_settings(state: &WebState, request_id: &str) -> ServerEvent {
    let Some(store) = store_of(state) else {
        return store_unavailable(request_id);
    };
    let result = async {
        let settings = store.list_settings().await?;
        Ok::<_, ApiError>(SettingsSnapshotView {
            providers: store
                .list_providers()
                .await?
                .into_iter()
                .map(ProviderView::from)
                .collect(),
            model_specs: store.list_model_specs().await?,
            settings,
            scalar_keys: keys::ALL,
        })
    }
    .await;
    match result {
        Ok(snapshot) => ServerEvent::SettingsSnapshot {
            request_id: request_id.to_string(),
            snapshot: Box::new(snapshot),
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 新建或更新 provider（逐字段补丁三态：字段缺失 = 不更新，null = 清除）。
pub async fn handle_upsert_provider(
    state: &WebState,
    request_id: &str,
    name: &str,
    patch: ProviderPatch,
) -> ServerEvent {
    let Some(store) = store_of(state) else {
        return store_unavailable(request_id);
    };
    let result = async {
        validate_provider_patch(&store, name, &patch).await?;
        store.upsert_provider(name, patch).await?;
        Ok::<_, ApiError>(())
    }
    .await;
    match result {
        Ok(()) => finish_mutation(state, request_id).await,
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 删除 provider（其模型覆盖级联清除）。
pub async fn handle_delete_provider(state: &WebState, request_id: &str, name: &str) -> ServerEvent {
    let Some(store) = store_of(state) else {
        return store_unavailable(request_id);
    };
    match store.delete_provider(name).await {
        Ok(_) => finish_mutation(state, request_id).await,
        Err(error) => ApiError::from(error).to_ws_response(Some(request_id)),
    }
}

/// 新建或更新模型覆盖（逐字段补丁三态同 provider）；所属 provider
/// 必须已定义（预检给出可读错误，外键约束兜底）。
pub async fn handle_upsert_model_spec(
    state: &WebState,
    request_id: &str,
    provider: &str,
    model_id: &str,
    patch: ModelSpecPatch,
) -> ServerEvent {
    let Some(store) = store_of(state) else {
        return store_unavailable(request_id);
    };
    let result = async {
        if model_id.is_empty() || model_id.contains(char::is_whitespace) {
            return Err(ApiError::BadRequest(format!(
                "模型 id {model_id:?} 非法：非空且不能含空白字符"
            )));
        }
        if store.get_provider(provider).await?.is_none() {
            return Err(ApiError::BadRequest(format!(
                "provider {provider:?} 未定义：先创建 provider 再写模型覆盖"
            )));
        }
        store.upsert_model_spec(provider, model_id, patch).await?;
        Ok::<_, ApiError>(())
    }
    .await;
    match result {
        Ok(()) => finish_mutation(state, request_id).await,
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 删除模型覆盖。
pub async fn handle_delete_model_spec(
    state: &WebState,
    request_id: &str,
    provider: &str,
    model_id: &str,
) -> ServerEvent {
    let Some(store) = store_of(state) else {
        return store_unavailable(request_id);
    };
    match store.delete_model_spec(provider, model_id).await {
        Ok(_) => finish_mutation(state, request_id).await,
        Err(error) => ApiError::from(error).to_ws_response(Some(request_id)),
    }
}

/// 写入标量设置（键与取值类型经 [`validate_scalar`] 校验，未知键硬报错）。
pub async fn handle_set_setting(
    state: &WebState,
    request_id: &str,
    key: &str,
    value: serde_json::Value,
) -> ServerEvent {
    let Some(store) = store_of(state) else {
        return store_unavailable(request_id);
    };
    if let Err(error) = validate_scalar(key, &value) {
        return ApiError::BadRequest(format!("{error:#}")).to_ws_response(Some(request_id));
    }
    match store.set_setting(key, &value).await {
        Ok(()) => finish_mutation(state, request_id).await,
        Err(error) => ApiError::from(error).to_ws_response(Some(request_id)),
    }
}

/// 删除标量设置（键须已知，未知键硬报错防拼写错误）。
pub async fn handle_unset_setting(state: &WebState, request_id: &str, key: &str) -> ServerEvent {
    let Some(store) = store_of(state) else {
        return store_unavailable(request_id);
    };
    if !keys::ALL.contains(&key) {
        return ApiError::BadRequest(format!(
            "未知设置键 {key:?}（可选：{}）",
            keys::ALL.join(" / ")
        ))
        .to_ws_response(Some(request_id));
    }
    match store.unset_setting(key).await {
        Ok(_) => finish_mutation(state, request_id).await,
        Err(error) => ApiError::from(error).to_ws_response(Some(request_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::tests::test_state;

    #[tokio::test]
    async fn get_settings_snapshot_roundtrip() {
        let state = test_state().await;
        let request_id = "r1";
        // 空库：快照各段为空
        let ServerEvent::SettingsSnapshot { snapshot, .. } =
            handle_get_settings(&state, request_id).await
        else {
            panic!("expected SettingsSnapshot");
        };
        assert!(snapshot.providers.is_empty());
        assert!(snapshot.settings.is_empty());
        assert!(snapshot.scalar_keys.contains(&"append_system"));

        // 写入后经快照可读回
        let store = state.inner.services.store().cloned().expect("store");
        store
            .set_setting("append_system", &serde_json::json!("保持简洁"))
            .await
            .expect("set");
        let ServerEvent::SettingsSnapshot { snapshot, .. } =
            handle_get_settings(&state, request_id).await
        else {
            panic!("expected SettingsSnapshot");
        };
        assert_eq!(
            snapshot.settings["append_system"],
            serde_json::json!("保持简洁")
        );
    }

    #[tokio::test]
    async fn upsert_provider_validates_and_broadcasts() {
        let state = test_state().await;
        let mut rx = state.inner.services.bus().subscribe();

        // 自定义 provider 不给 api：BadRequest
        let event =
            handle_upsert_provider(&state, "r2", "deepseek", ProviderPatch::default()).await;
        assert!(matches!(event, ServerEvent::Error { .. }), "{event:?}");

        // 合法写入：ack + 广播
        let event = handle_upsert_provider(
            &state,
            "r3",
            "deepseek",
            ProviderPatch {
                api: Some(Some(nomic_ai::ApiKind::OpenAiCompletions)),
                base_url: Some(Some("https://api.deepseek.com/v1".to_string())),
                api_key: None,
            },
        )
        .await;
        assert!(
            matches!(&event, ServerEvent::SettingsUpdated { request_id } if request_id == "r3"),
            "{event:?}"
        );
        let mut saw_changed = false;
        while let Ok(event) = rx.try_recv() {
            saw_changed |= matches!(event, ServerEvent::SettingsChanged);
        }
        assert!(saw_changed, "写后应广播 SettingsChanged");

        // 快照 reload 生效：模型解析器立即可见新 provider
        assert!(
            state
                .inner
                .services
                .models()
                .provider_row("deepseek")
                .is_some()
        );
    }

    #[tokio::test]
    async fn set_setting_validates_key_and_value() {
        let state = test_state().await;
        // 未知键（含已废弃键）
        let event = handle_set_setting(&state, "r4", "no_such_key", serde_json::json!(1)).await;
        assert!(matches!(event, ServerEvent::Error { .. }));
        let event = handle_set_setting(&state, "r4b", "temperature", serde_json::json!(0.7)).await;
        assert!(matches!(event, ServerEvent::Error { .. }));
        // 类型不符
        let event =
            handle_set_setting(&state, "r5", "compaction.enabled", serde_json::json!("是")).await;
        assert!(matches!(event, ServerEvent::Error { .. }));
        // 合法写入 + unset
        let event =
            handle_set_setting(&state, "r6", "compaction.enabled", serde_json::json!(false)).await;
        assert!(matches!(event, ServerEvent::SettingsUpdated { .. }));
        let event = handle_unset_setting(&state, "r7", "compaction.enabled").await;
        assert!(matches!(event, ServerEvent::SettingsUpdated { .. }));
        let event = handle_unset_setting(&state, "r8", "no_such_key").await;
        assert!(matches!(event, ServerEvent::Error { .. }));
    }

    #[tokio::test]
    async fn model_spec_requires_defined_provider() {
        let state = test_state().await;
        let event =
            handle_upsert_model_spec(&state, "r9", "ghost", "m", ModelSpecPatch::default()).await;
        assert!(matches!(event, ServerEvent::Error { .. }), "{event:?}");

        handle_upsert_provider(&state, "r10", "openai", ProviderPatch::default()).await;
        let event = handle_upsert_model_spec(
            &state,
            "r11",
            "openai",
            "gpt-5.2",
            ModelSpecPatch {
                context_window: Some(Some(400_000)),
                ..ModelSpecPatch::default()
            },
        )
        .await;
        assert!(matches!(event, ServerEvent::SettingsUpdated { .. }));
        let event = handle_delete_model_spec(&state, "r12", "openai", "gpt-5.2").await;
        assert!(matches!(event, ServerEvent::SettingsUpdated { .. }));
    }
}
