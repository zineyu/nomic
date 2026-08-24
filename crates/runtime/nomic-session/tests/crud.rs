//! session / workspace 管理操作：删除（含级联与 force 语义）与自定义标题。

use nomic_ai::{Message, UserMessage, UserMessageContent};
use nomic_session::{SessionError, SessionStore};

fn user_message(text: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp,
    })
}

// ── delete_session ────────────────────────────────────────────────────────

#[tokio::test]
async fn delete_session_cascades_entries_and_session_config() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/ws").await.unwrap();
    store
        .append_message(&session, None, &user_message("hi", 1_000))
        .await
        .unwrap();
    store
        .set_session_config(&session, "model", &serde_json::json!("openai/gpt-4o"))
        .await
        .unwrap();

    assert!(store.delete_session(&session).await.unwrap());
    // entries / 会话级 config 级联清除
    assert!(
        matches!(
            store.load_messages(&session).await,
            Err(SessionError::SessionNotFound(_))
        ),
        "删除后加载历史应报 SessionNotFound"
    );
    assert!(
        store
            .get_session_config::<String>(&session, "model")
            .await
            .unwrap()
            .is_none(),
        "会话级 config 应级联清除"
    );
    // 幂等：重复删除返回 false
    assert!(!store.delete_session(&session).await.unwrap());
    // 列表不再出现
    assert!(store.list_sessions().await.unwrap().is_empty());
}

#[tokio::test]
async fn delete_session_keeps_sibling_sessions() {
    let store = SessionStore::in_memory().await.unwrap();
    let a = store.create_session("/tmp/ws").await.unwrap();
    let b = store.create_session("/tmp/ws").await.unwrap();
    store
        .append_message(&a, None, &user_message("a", 1_000))
        .await
        .unwrap();
    store
        .append_message(&b, None, &user_message("b", 2_000))
        .await
        .unwrap();

    assert!(store.delete_session(&a).await.unwrap());
    let rest = store.list_sessions().await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].id, b, "同 workspace 的其他 session 不受影响");
}

// ── rename_session / 自定义标题 ───────────────────────────────────────────

#[tokio::test]
async fn rename_session_overrides_derived_title() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/ws").await.unwrap();
    store
        .append_message(&session, None, &user_message("原始标题", 1_000))
        .await
        .unwrap();

    // 派生标题兜底
    assert_eq!(
        store.session_title_override(&session).await.unwrap(),
        None
    );
    assert_eq!(
        store.list_sessions().await.unwrap()[0].title.as_deref(),
        Some("原始标题")
    );

    // 自定义标题优先（首尾空白裁剪）
    let renamed = store
        .rename_session(&session, "  我的会话  ")
        .await
        .unwrap();
    assert_eq!(renamed.as_deref(), Some("我的会话"));
    assert_eq!(
        store.session_title_override(&session).await.unwrap(),
        Some("我的会话".to_string())
    );
    assert_eq!(
        store.list_sessions().await.unwrap()[0].title.as_deref(),
        Some("我的会话")
    );

    // 空白标题 = 清除自定义，回退派生标题
    let cleared = store.rename_session(&session, "   ").await.unwrap();
    assert_eq!(cleared, None);
    assert_eq!(
        store.list_sessions().await.unwrap()[0].title.as_deref(),
        Some("原始标题")
    );
}

#[tokio::test]
async fn rename_session_rejects_unknown_session() {
    let store = SessionStore::in_memory().await.unwrap();
    assert!(matches!(
        store.rename_session("no-such-session", "x").await,
        Err(SessionError::SessionNotFound(_))
    ));
    // 不存在 session 的自定义标题查询为 None（不报错）
    assert_eq!(
        store.session_title_override("no-such-session").await.unwrap(),
        None
    );
}

// ── delete_workspace ──────────────────────────────────────────────────────

#[tokio::test]
async fn delete_workspace_removes_empty_workspace() {
    let store = SessionStore::in_memory().await.unwrap();
    let workspace = store.get_or_create_workspace("/tmp/ws").await.unwrap();

    assert!(store.delete_workspace(&workspace.id, false).await.unwrap());
    assert!(store.workspace(&workspace.id).await.unwrap().is_none());
    // 幂等：重复删除返回 false
    assert!(!store.delete_workspace(&workspace.id, false).await.unwrap());
}

#[tokio::test]
async fn delete_workspace_refuses_sessions_with_user_messages() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/ws").await.unwrap();
    store
        .append_message(&session, None, &user_message("hi", 1_000))
        .await
        .unwrap();
    let workspace = store.workspace_of_session(&session).await.unwrap().unwrap();

    let Err(SessionError::WorkspaceNotEmpty { id, count }) =
        store.delete_workspace(&workspace.id, false).await
    else {
        panic!("非空 workspace 应拒绝删除");
    };
    assert_eq!(id, workspace.id);
    assert_eq!(count, 1);
    // 数据保持原样
    assert_eq!(store.list_sessions().await.unwrap().len(), 1);
    assert!(store.workspace(&workspace.id).await.unwrap().is_some());
}

#[tokio::test]
async fn delete_workspace_allows_shell_only_workspace() {
    let store = SessionStore::in_memory().await.unwrap();
    // 只有空壳 session（无 user 消息）：不进列表口径，不拦截删除
    let shell = store.create_session("/tmp/ws").await.unwrap();
    let workspace = store.workspace_of_session(&shell).await.unwrap().unwrap();

    assert!(store.delete_workspace(&workspace.id, false).await.unwrap());
    assert!(store.workspace(&workspace.id).await.unwrap().is_none());
    assert!(
        matches!(
            store.load_messages(&shell).await,
            Err(SessionError::SessionNotFound(_))
        ),
        "空壳 session 应随 workspace 一并清除"
    );
}

#[tokio::test]
async fn delete_workspace_force_cascades_sessions() {
    let store = SessionStore::in_memory().await.unwrap();
    let a = store.create_session("/tmp/ws").await.unwrap();
    let b = store.create_session("/tmp/ws").await.unwrap();
    let other = store.create_session("/tmp/other").await.unwrap();
    for (id, text) in [(&a, "a"), (&b, "b"), (&other, "other")] {
        store
            .append_message(id, None, &user_message(text, 1_000))
            .await
            .unwrap();
    }
    let workspace = store.workspace_of_session(&a).await.unwrap().unwrap();

    assert!(store.delete_workspace(&workspace.id, true).await.unwrap());
    assert!(store.workspace(&workspace.id).await.unwrap().is_none());
    let rest = store.list_sessions().await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].id, other, "其他 workspace 的 session 不受影响");
}
