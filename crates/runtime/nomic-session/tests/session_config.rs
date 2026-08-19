//! 会话级 config 集成测试：与全局 config 隔离、按 session 回退、外键约束。

use nomic_session::{SessionError, SessionStore};

#[tokio::test]
async fn session_config_is_isolated_from_global_config() {
    let store = SessionStore::in_memory().await.unwrap();
    let a = store.create_session(".").await.unwrap();
    let b = store.create_session(".").await.unwrap();

    store
        .set_config("model", &serde_json::json!("openai/gpt-5.2"))
        .await
        .unwrap();
    store
        .set_session_config(
            &a,
            "model",
            &serde_json::json!("anthropic/claude-sonnet-4-5"),
        )
        .await
        .unwrap();
    store
        .set_session_config(&b, "model", &serde_json::json!("openai/gpt-5.2-mini"))
        .await
        .unwrap();

    // 全局读取只看到全局行，不混入会话级覆盖
    assert_eq!(
        store.config_history("model").await.unwrap(),
        vec![serde_json::json!("openai/gpt-5.2")]
    );
    // 会话读取只看到自己的覆盖，看不到全局或其他会话
    assert_eq!(
        store.session_config_history(&a, "model").await.unwrap(),
        vec![serde_json::json!("anthropic/claude-sonnet-4-5")]
    );
    assert_eq!(
        store.session_config_history(&b, "model").await.unwrap(),
        vec![serde_json::json!("openai/gpt-5.2-mini")]
    );
}

#[tokio::test]
async fn session_config_history_is_append_only_newest_first() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session(".").await.unwrap();

    store
        .set_session_config(&session, "model", &serde_json::json!("openai/gpt-5.2"))
        .await
        .unwrap();
    store
        .set_session_config(&session, "model", &serde_json::json!("openai/gpt-5.2-mini"))
        .await
        .unwrap();

    let history = store
        .session_config_history(&session, "model")
        .await
        .unwrap();
    assert_eq!(
        history,
        vec![
            serde_json::json!("openai/gpt-5.2-mini"),
            serde_json::json!("openai/gpt-5.2"),
        ],
        "会话级配置最新在前、同 key 追加"
    );

    // get_session_config 取最新、类型不符回退
    store
        .set_session_config(&session, "model", &serde_json::json!(42))
        .await
        .unwrap();
    let selected: Option<String> = store.get_session_config(&session, "model").await.unwrap();
    assert_eq!(selected.as_deref(), Some("openai/gpt-5.2-mini"));
}

#[tokio::test]
async fn session_config_rejects_unknown_session() {
    let store = SessionStore::in_memory().await.unwrap();
    let result = store
        .set_session_config("no-such-session", "model", &serde_json::json!("x/y"))
        .await;
    assert!(
        matches!(result, Err(SessionError::Sqlx(_))),
        "外键约束拒绝不存在的 session"
    );
}
