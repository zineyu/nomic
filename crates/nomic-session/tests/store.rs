//! SessionStore 集成测试：临时文件库 + 内存库，无网络/外部依赖。

use std::path::Path;

use nomic_ai::{
    ApiKind, AssistantContent, AssistantMessage, Message, StopReason, TextContent, ThinkingContent,
    ToolCall, ToolResultMessage, Usage, UserContent, UserMessage, UserMessageContent,
};
use nomic_session::{SessionError, SessionStore};

fn user_message(text: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp,
    })
}

/// 含 thinking 签名、工具调用、usage/cost 的完整 assistant 消息（验证 payload 保真）。
fn assistant_message(timestamp: u64) -> Message {
    let mut usage = Usage {
        input: 100,
        output: 42,
        cache_read: 10,
        cache_write: 5,
        reasoning: Some(7),
        total_tokens: 157,
        ..Usage::default()
    };
    usage.cost.total = 0.001;
    Message::Assistant(AssistantMessage {
        content: vec![
            AssistantContent::Thinking(ThinkingContent {
                thinking: "let me think".to_string(),
                thinking_signature: Some("sig-abc".to_string()),
                redacted: false,
            }),
            AssistantContent::Text(TextContent {
                text: "done".to_string(),
                text_signature: None,
            }),
            AssistantContent::ToolCall(ToolCall {
                id: "call-1".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "/tmp/a.rs", "offset": 1}),
                thought_signature: None,
            }),
        ],
        api: ApiKind::AnthropicMessages,
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-5".to_string(),
        response_model: None,
        response_id: Some("resp-1".to_string()),
        usage,
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp,
    })
}

fn tool_result_message(timestamp: u64) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: "call-1".to_string(),
        tool_name: "read".to_string(),
        content: vec![UserContent::Text(TextContent {
            text: "file contents".to_string(),
            text_signature: None,
        })],
        details: Some(serde_json::json!({"truncated": false})),
        is_error: false,
        timestamp,
    })
}

async fn open_temp(dir: &Path) -> (SessionStore, std::path::PathBuf) {
    let path = dir.join("nested").join("sessions.db");
    let store = SessionStore::open(&path).await.expect("open temp db");
    (store, path)
}

#[tokio::test]
async fn open_creates_schema_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let (store, path) = open_temp(dir.path()).await;
    drop(store);
    assert!(path.exists(), "db 文件应被创建（含自动建父目录）");
    // 重复 open：迁移幂等，不报错
    let _store = SessionStore::open(&path).await.expect("reopen");
}

#[tokio::test]
async fn create_session_records_cwd_and_null_timestamps() {
    let store = SessionStore::in_memory().await.unwrap();
    let id_a = store.create_session("/tmp/project-a").await.unwrap();
    let id_b = store.create_session("/tmp/project-b").await.unwrap();

    assert_ne!(id_a, id_b, "session id 应互不相同");
    uuid::Uuid::parse_str(&id_a).expect("session id 应为合法 UUID");

    let summaries = store.list_sessions().await.unwrap();
    let a = summaries.iter().find(|s| s.id == id_a).unwrap();
    assert_eq!(a.cwd, Path::new("/tmp/project-a"));
    assert_eq!(a.first_message_at, None);
    assert_eq!(a.last_message_at, None);
    assert_eq!(a.message_count, 0);
}

#[tokio::test]
async fn append_maintains_first_and_last_message_time() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();

    store
        .append_message(&session, None, &user_message("hi", 1_000))
        .await
        .unwrap();
    store
        .append_message(&session, None, &assistant_message(2_000))
        .await
        .unwrap();
    store
        .append_message(&session, None, &tool_result_message(3_000))
        .await
        .unwrap();

    let summaries = store.list_sessions().await.unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].first_message_at, Some(1_000));
    assert_eq!(summaries[0].last_message_at, Some(3_000));
    assert_eq!(summaries[0].message_count, 3);
}

#[tokio::test]
async fn messages_roundtrip_without_loss() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();

    let messages = vec![
        user_message("hi", 1_000),
        assistant_message(2_000),
        tool_result_message(3_000),
    ];
    for message in &messages {
        store.append_message(&session, None, message).await.unwrap();
    }

    let loaded = store.load_messages(&session).await.unwrap();
    assert_eq!(loaded, messages, "payload JSON 应无损 roundtrip");
}

#[tokio::test]
async fn tree_branch_defaults_to_latest_child() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();

    let root = store
        .append_message(&session, None, &user_message("root", 1_000))
        .await
        .unwrap();
    // 同一 parent 两个子节点：默认分支取最新子节点
    store
        .append_message(&session, Some(&root), &user_message("branch A", 2_000))
        .await
        .unwrap();
    store
        .append_message(&session, Some(&root), &user_message("branch B", 3_000))
        .await
        .unwrap();

    let loaded = store.load_messages(&session).await.unwrap();
    assert_eq!(
        loaded,
        vec![user_message("root", 1_000), user_message("branch B", 3_000)],
        "默认分支应沿最新子节点走"
    );
}

#[tokio::test]
async fn sequential_append_chains_automatically() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();

    store
        .append_message(&session, None, &user_message("one", 1_000))
        .await
        .unwrap();
    store
        .append_message(&session, None, &user_message("two", 2_000))
        .await
        .unwrap();

    // parent=None 自动链到最新 entry，load 应得到完整顺序序列而非分支
    let loaded = store.load_messages(&session).await.unwrap();
    assert_eq!(
        loaded,
        vec![user_message("one", 1_000), user_message("two", 2_000)]
    );
}

#[tokio::test]
async fn persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (store, path) = open_temp(dir.path()).await;
    let session = store.create_session("/tmp/p").await.unwrap();
    store
        .append_message(&session, None, &assistant_message(1_000))
        .await
        .unwrap();
    drop(store);

    let reopened = SessionStore::open(&path).await.unwrap();
    let loaded = reopened.load_messages(&session).await.unwrap();
    assert_eq!(loaded, vec![assistant_message(1_000)]);
    let summaries = reopened.list_sessions().await.unwrap();
    assert_eq!(summaries[0].message_count, 1);
}

#[tokio::test]
async fn append_to_missing_session_fails() {
    let store = SessionStore::in_memory().await.unwrap();
    let err = store
        .append_message("no-such-session", None, &user_message("hi", 1_000))
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::SessionNotFound(_)));
}

#[tokio::test]
async fn load_missing_session_fails() {
    let store = SessionStore::in_memory().await.unwrap();
    let err = store.load_messages("no-such-session").await.unwrap_err();
    assert!(matches!(err, SessionError::SessionNotFound(_)));
}

#[tokio::test]
async fn append_with_missing_parent_fails() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    let err = store
        .append_message(&session, Some("no-such-entry"), &user_message("hi", 1_000))
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::EntryNotFound(_)));
}

#[tokio::test]
async fn list_sessions_orders_by_last_message_desc() {
    let store = SessionStore::in_memory().await.unwrap();
    let empty = store.create_session("/tmp/empty").await.unwrap();
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
        vec![newer.as_str(), older.as_str(), empty.as_str()],
        "应按末条消息时间降序，无消息的排最后"
    );
}
