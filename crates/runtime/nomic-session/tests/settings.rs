//! 设置表（providers / model_specs / settings，ADR-0039）的 CRUD 集成测试：
//! 逐字段补丁三态语义、外键级联、upsert / unset 往返。

use nomic_ai::ApiKind;
use nomic_session::{ModelSpecPatch, ProviderPatch, SessionStore};

#[tokio::test]
async fn provider_upsert_patch_semantics() {
    let store = SessionStore::in_memory().await.unwrap();
    assert!(store.list_providers().await.unwrap().is_empty());
    assert!(store.get_provider("anthropic").await.unwrap().is_none());

    // 新建：补丁字段落库，未提及字段为 None
    let row = store
        .upsert_provider(
            "anthropic",
            ProviderPatch {
                base_url: Some(Some("https://api.anthropic.com".to_string())),
                api_key: Some(Some("sk-ant-test".to_string())),
                ..ProviderPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.api, None);
    assert_eq!(row.base_url.as_deref(), Some("https://api.anthropic.com"));
    assert!(row.updated_at > 0);

    // 更新：外层 None 不触碰，Some(Some) 设置，Some(None) 清除
    let row = store
        .upsert_provider(
            "anthropic",
            ProviderPatch {
                api: Some(Some(ApiKind::AnthropicMessages)),
                api_key: Some(None),
                ..ProviderPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.api, Some(ApiKind::AnthropicMessages));
    assert_eq!(
        row.base_url.as_deref(),
        Some("https://api.anthropic.com"),
        "未提及的字段保持原值"
    );
    assert_eq!(row.api_key, None, "Some(None) 清除字段");

    let providers = store.list_providers().await.unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].name, "anthropic");
}

#[tokio::test]
async fn delete_provider_cascades_model_specs() {
    let store = SessionStore::in_memory().await.unwrap();
    store
        .upsert_provider("openai", ProviderPatch::default())
        .await
        .unwrap();
    store
        .upsert_model_spec(
            "openai",
            "gpt-5.2",
            ModelSpecPatch {
                context_window: Some(Some(400_000)),
                ..ModelSpecPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(store.list_model_specs().await.unwrap().len(), 1);

    assert!(store.delete_provider("openai").await.unwrap());
    assert!(!store.delete_provider("openai").await.unwrap());
    assert!(
        store.list_model_specs().await.unwrap().is_empty(),
        "provider 删除后其模型规格覆盖级联清除"
    );
}

#[tokio::test]
async fn model_spec_requires_existing_provider() {
    let store = SessionStore::in_memory().await.unwrap();
    let result = store
        .upsert_model_spec("ghost", "m", ModelSpecPatch::default())
        .await;
    assert!(result.is_err(), "provider 不存在时外键约束拒绝写入");
}

#[tokio::test]
async fn model_spec_upsert_patch_semantics() {
    let store = SessionStore::in_memory().await.unwrap();
    store
        .upsert_provider("anthropic", ProviderPatch::default())
        .await
        .unwrap();

    let row = store
        .upsert_model_spec(
            "anthropic",
            "claude-sonnet-4-5",
            ModelSpecPatch {
                name: Some(Some("Claude Sonnet 4.5".to_string())),
                reasoning: Some(Some(true)),
                vision: Some(Some(true)),
                context_window: Some(Some(200_000)),
                max_tokens: Some(Some(64_000)),
                cost_input: Some(Some(3.0)),
                cost_output: Some(Some(15.0)),
                cost_cache_read: Some(Some(0.3)),
                cost_cache_write: Some(Some(3.75)),
            },
        )
        .await
        .unwrap();
    assert!(row.spec.is_complete(), "全字段写入后规格完整");
    assert_eq!(row.spec.context_window, Some(200_000));
    assert_eq!(row.spec.cost_cache_write, Some(3.75));

    // 逐字段补丁：清除 name、其余保持
    let row = store
        .upsert_model_spec(
            "anthropic",
            "claude-sonnet-4-5",
            ModelSpecPatch {
                name: Some(None),
                ..ModelSpecPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(row.spec.name, None);
    assert_eq!(row.spec.context_window, Some(200_000), "未提及字段保持");
    assert!(!row.spec.is_complete());

    assert!(
        store
            .delete_model_spec("anthropic", "claude-sonnet-4-5")
            .await
            .unwrap()
    );
    assert!(
        !store
            .delete_model_spec("anthropic", "claude-sonnet-4-5")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn settings_set_get_unset_roundtrip() {
    let store = SessionStore::in_memory().await.unwrap();
    assert!(
        store
            .get_setting::<f64>("temperature")
            .await
            .unwrap()
            .is_none()
    );

    store
        .set_setting("temperature", &serde_json::json!(0.7))
        .await
        .unwrap();
    assert_eq!(
        store.get_setting::<f64>("temperature").await.unwrap(),
        Some(0.7)
    );

    // upsert 语义：覆盖而非追加
    store
        .set_setting("temperature", &serde_json::json!(0.2))
        .await
        .unwrap();
    assert_eq!(
        store.get_setting::<f64>("temperature").await.unwrap(),
        Some(0.2)
    );
    assert_eq!(store.list_settings().await.unwrap().len(), 1);

    // 类型不符返回 None（不报错）
    assert!(
        store
            .get_setting::<String>("temperature")
            .await
            .unwrap()
            .is_none()
    );

    store
        .set_setting("append_system", &serde_json::json!("总是用中文回复。"))
        .await
        .unwrap();
    let all = store.list_settings().await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all["append_system"], serde_json::json!("总是用中文回复。"));

    assert!(store.unset_setting("temperature").await.unwrap());
    assert!(!store.unset_setting("temperature").await.unwrap());
    assert!(
        store
            .get_setting::<f64>("temperature")
            .await
            .unwrap()
            .is_none()
    );
}
