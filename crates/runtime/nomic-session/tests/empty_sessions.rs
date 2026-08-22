//! 空壳 session（无 user 消息）语义：不进入列表/统计口径，可物理删除。

use nomic_ai::{
    ApiKind, AssistantContent, AssistantMessage, Message, StopReason, TextContent, Usage,
    UserMessage, UserMessageContent,
};
use nomic_session::{SessionError, SessionStore};

fn user_message(text: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp,
    })
}

/// 最小 assistant 消息（空壳判定只关心 role，内容从简）。
fn assistant_message(timestamp: u64) -> Message {
    Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::Text(TextContent {
            text: "done".to_string(),
            text_signature: None,
        })],
        api: ApiKind::AnthropicMessages,
        provider: "anthropic".to_string(),
        model: "test-model".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp,
    })
}

#[tokio::test]
async fn list_sessions_excludes_sessions_without_user_messages() {
    let store = SessionStore::in_memory().await.unwrap();
    let empty = store.create_session("/tmp/empty").await.unwrap();
    let assistant_only = store.create_session("/tmp/assistant-only").await.unwrap();
    let active = store.create_session("/tmp/active").await.unwrap();

    store
        .append_message(&assistant_only, None, &assistant_message(1_000))
        .await
        .unwrap();
    store
        .append_message(&active, None, &user_message("hi", 2_000))
        .await
        .unwrap();

    let summaries = store.list_sessions().await.unwrap();
    let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![active.as_str()],
        "无 user 消息的 session（空的 / 仅 assistant 的）不进入列表"
    );
    // workspace 过滤口径一致
    let workspace = store.workspace_of_session(&empty).await.unwrap().unwrap();
    assert!(
        store
            .list_sessions_in(&workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn list_workspaces_counts_only_sessions_with_user_messages() {
    let store = SessionStore::in_memory().await.unwrap();
    let empty = store.create_session("/tmp/ws").await.unwrap();
    let active = store.create_session("/tmp/ws").await.unwrap();
    store
        .append_message(&active, None, &user_message("hi", 1_000))
        .await
        .unwrap();

    let workspace = store.workspace_of_session(&empty).await.unwrap().unwrap();
    let workspaces = store.list_workspaces().await.unwrap();
    let ws = workspaces.iter().find(|w| w.id == workspace.id).unwrap();
    assert_eq!(ws.session_count, 1, "空壳 session 不计入 workspace 统计");
}

#[tokio::test]
async fn delete_if_no_user_message_removes_empty_session_and_cascades() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    // 仅有非 user 条目与会话级配置：仍属空壳，删除时级联清除
    store
        .append_message(&session, None, &assistant_message(1_000))
        .await
        .unwrap();
    store
        .set_session_config(&session, "model", &serde_json::json!("openai/gpt-5.2"))
        .await
        .unwrap();

    let deleted = store.delete_if_no_user_message(&session).await.unwrap();
    assert!(deleted, "无 user 消息的 session 应被删除");

    let result = store.load_messages(&session).await;
    assert!(matches!(result, Err(SessionError::SessionNotFound(_))));
    assert!(
        store
            .session_config_history(&session, "model")
            .await
            .unwrap()
            .is_empty(),
        "会话级配置应随 session 级联删除"
    );
}

#[tokio::test]
async fn delete_if_no_user_message_keeps_session_with_user_message() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    store
        .append_message(&session, None, &user_message("hi", 1_000))
        .await
        .unwrap();

    let deleted = store.delete_if_no_user_message(&session).await.unwrap();
    assert!(!deleted, "有 user 消息的 session 不应被删除");
    assert_eq!(store.load_messages(&session).await.unwrap().len(), 1);

    // 不存在的 session：no-op
    let deleted = store
        .delete_if_no_user_message("no-such-session")
        .await
        .unwrap();
    assert!(!deleted);
}
