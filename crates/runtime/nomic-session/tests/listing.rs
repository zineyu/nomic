//! session 摘要列表（`list_sessions` / `list_sessions_in`）：排序、标题口径。
//! 空壳 session（无 user 消息）的过滤与清理见 `tests/empty_sessions.rs`。

use nomic_ai::{
    ApiKind, AssistantContent, AssistantMessage, Message, StopReason, TextContent, Usage,
    UserMessage, UserMessageContent,
};
use nomic_session::SessionStore;

fn user_message(text: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp,
    })
}

/// 最小 assistant 消息（列表口径只关心 role 与时间，内容从简）。
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
async fn list_sessions_orders_by_last_message_desc() {
    let store = SessionStore::in_memory().await.unwrap();
    let older = store.create_session("/tmp/older").await.unwrap();
    let newer = store.create_session("/tmp/newer").await.unwrap();

    store
        .append_message(&older, None, &user_message("old", 1_000))
        .await
        .unwrap();
    store
        .append_message(&newer, None, &user_message("new", 2_000))
        .await
        .unwrap();

    let summaries = store.list_sessions().await.unwrap();
    let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![newer.as_str(), older.as_str()],
        "应按末条消息时间降序"
    );
}

#[tokio::test]
async fn list_sessions_computes_title_from_first_user_message() {
    let store = SessionStore::in_memory().await.unwrap();
    let titled = store.create_session("/tmp/titled").await.unwrap();

    // 标题取首条 user 消息的首行，即使它不是 session 的第一条 entry
    store
        .append_message(&titled, None, &assistant_message(1_000))
        .await
        .unwrap();
    store
        .append_message(
            &titled,
            None,
            &user_message("实现会话命名\n第二行忽略", 2_000),
        )
        .await
        .unwrap();
    store
        .append_message(&titled, None, &user_message("后来的消息不作标题", 3_000))
        .await
        .unwrap();

    let summaries = store.list_sessions().await.unwrap();
    let titled_summary = summaries.iter().find(|s| s.id == titled).unwrap();
    assert_eq!(titled_summary.title.as_deref(), Some("实现会话命名"));
}
