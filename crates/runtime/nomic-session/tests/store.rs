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
async fn create_session_binds_workspace_and_null_timestamps() {
    let store = SessionStore::in_memory().await.unwrap();
    let id_a = store.create_session("/tmp/project-a").await.unwrap();
    let id_b = store.create_session("/tmp/project-b").await.unwrap();

    assert_ne!(id_a, id_b, "session id 应互不相同");
    uuid::Uuid::parse_str(&id_a).expect("session id 应为合法 UUID");

    // 无 user 消息的 session 不进入列表口径；归属经 workspace 查询验证
    assert!(store.list_sessions().await.unwrap().is_empty());
    let workspace_a = store.workspace_of_session(&id_a).await.unwrap().unwrap();
    assert_eq!(workspace_a.path, Path::new("/tmp/project-a"));

    // 同一路径创建第二个 session：复用同一 workspace，不重复登记
    let id_a2 = store.create_session("/tmp/project-a").await.unwrap();
    let workspace_a2 = store.workspace_of_session(&id_a2).await.unwrap().unwrap();
    assert_eq!(workspace_a2.id, workspace_a.id);
    let workspaces = store.list_workspaces().await.unwrap();
    assert_eq!(workspaces.len(), 2);
    let wa = workspaces.iter().find(|w| w.id == workspace_a.id).unwrap();
    assert_eq!(wa.path, Path::new("/tmp/project-a"));
    assert_eq!(wa.session_count, 0, "空壳 session 不计入统计");
    assert!(wa.last_active_at.is_some(), "创建 session 推进活跃时间");

    // 有 user 消息后进入列表与统计：时间字段与消息数照常维护
    store
        .append_message(&id_a, None, &user_message("hi", 1_000))
        .await
        .unwrap();
    let summaries = store.list_sessions().await.unwrap();
    let a = summaries.iter().find(|s| s.id == id_a).unwrap();
    assert_eq!(a.workspace, Path::new("/tmp/project-a"));
    assert_eq!(a.first_message_at, Some(1_000));
    assert_eq!(a.last_message_at, Some(1_000));
    assert_eq!(a.message_count, 1);
    let workspaces = store.list_workspaces().await.unwrap();
    let wa = workspaces.iter().find(|w| w.id == workspace_a.id).unwrap();
    assert_eq!(wa.session_count, 1);
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
    // 只有 assistant 消息的 session 不进入列表口径（无 user 消息）
    assert!(reopened.list_sessions().await.unwrap().is_empty());
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

// ── compaction entry ────────────────────────────────────────────────────────

use nomic_ai::{extract_summary, is_summary_message};
use nomic_session::CompactionRecord;

fn compaction(summary: &str, kept_count: u64) -> CompactionRecord {
    CompactionRecord {
        summary: summary.to_string(),
        kept_count,
        tokens_before: 12_345,
    }
}

#[tokio::test]
async fn compaction_rebuilds_summary_plus_kept_tail() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    for (index, timestamp) in [(1, 1_000), (2, 2_000), (3, 3_000), (4, 4_000)] {
        store
            .append_message(
                &session,
                None,
                &user_message(&format!("m{index}"), timestamp),
            )
            .await
            .unwrap();
    }
    store
        .append_compaction(&session, None, &compaction("summary of m1+m2", 2))
        .await
        .unwrap();

    let loaded = store.load_messages(&session).await.unwrap();
    assert_eq!(loaded.len(), 3, "摘要 + 保留的 2 条尾部消息");
    assert!(is_summary_message(&loaded[0]));
    assert_eq!(extract_summary(&loaded[0]), Some("summary of m1+m2"));
    assert_eq!(loaded[1], user_message("m3", 3_000));
    assert_eq!(loaded[2], user_message("m4", 4_000));
}

#[tokio::test]
async fn messages_after_compaction_chain_and_survive_rebuild() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    store
        .append_message(&session, None, &user_message("old", 1_000))
        .await
        .unwrap();
    store
        .append_compaction(&session, None, &compaction("summary", 0))
        .await
        .unwrap();
    // 压缩后的新消息链在 compaction entry 之后
    store
        .append_message(&session, None, &user_message("new", 2_000))
        .await
        .unwrap();

    let loaded = store.load_messages(&session).await.unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(is_summary_message(&loaded[0]));
    assert_eq!(loaded[1], user_message("new", 2_000));
}

#[tokio::test]
async fn repeated_compaction_rebuilds_recursively() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    for (index, timestamp) in [(1, 1_000), (2, 2_000), (3, 3_000)] {
        store
            .append_message(
                &session,
                None,
                &user_message(&format!("m{index}"), timestamp),
            )
            .await
            .unwrap();
    }
    // 第一次压缩：有效上下文 = [summary1, m3]
    store
        .append_compaction(&session, None, &compaction("summary one", 1))
        .await
        .unwrap();
    store
        .append_message(&session, None, &user_message("m4", 4_000))
        .await
        .unwrap();
    // 第二次压缩：相对当时的有效上下文 [summary1, m3, m4] 保留 2 条
    store
        .append_compaction(&session, None, &compaction("summary two", 2))
        .await
        .unwrap();

    let loaded = store.load_messages(&session).await.unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(extract_summary(&loaded[0]), Some("summary two"));
    assert_eq!(loaded[1], user_message("m3", 3_000));
    assert_eq!(loaded[2], user_message("m4", 4_000));
}

#[tokio::test]
async fn compaction_kept_count_clamps_to_effective_len() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    store
        .append_message(&session, None, &user_message("only", 1_000))
        .await
        .unwrap();
    store
        .append_compaction(&session, None, &compaction("summary", 100))
        .await
        .unwrap();

    let loaded = store.load_messages(&session).await.unwrap();
    assert_eq!(loaded.len(), 2, "kept_count 超过有效长度时钳制");
    assert!(is_summary_message(&loaded[0]));
    assert_eq!(loaded[1], user_message("only", 1_000));
}

#[tokio::test]
async fn compaction_entry_not_counted_as_message() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    store
        .append_message(&session, None, &user_message("hi", 1_000))
        .await
        .unwrap();
    store
        .append_compaction(&session, None, &compaction("summary", 1))
        .await
        .unwrap();

    let summaries = store.list_sessions().await.unwrap();
    assert_eq!(summaries[0].message_count, 1, "压缩条目不计入消息数");
}

#[tokio::test]
async fn compaction_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open(&path).await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    store
        .append_message(&session, None, &user_message("hi", 1_000))
        .await
        .unwrap();
    store
        .append_compaction(&session, None, &compaction("summary", 0))
        .await
        .unwrap();

    let reopened = SessionStore::open(&path).await.unwrap();
    let loaded = reopened.load_messages(&session).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(extract_summary(&loaded[0]), Some("summary"));
}

// ── 分支浏览与加载（list_tree / load_branch / latest_entry_id）──────────────

/// 构造分支场景：
/// root ── a1（分支 A 叶子）
///      └─ a2 ── tool（分支 B 叶子，全局最新 entry）
async fn branched_store() -> (SessionStore, String, Branch) {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    let root = store
        .append_message(&session, None, &user_message("root", 1_000))
        .await
        .unwrap();
    let a1 = store
        .append_message(&session, Some(&root), &user_message("branch A", 2_000))
        .await
        .unwrap();
    let a2 = store
        .append_message(&session, Some(&root), &user_message("branch B", 3_000))
        .await
        .unwrap();
    let tool = store
        .append_message(&session, Some(&a2), &tool_result_message(4_000))
        .await
        .unwrap();
    (store, session, Branch { root, a1, a2, tool })
}

struct Branch {
    root: String,
    a1: String,
    a2: String,
    tool: String,
}

#[tokio::test]
async fn load_branch_replays_ancestor_path() {
    let (store, session, branch) = branched_store().await;

    // 非默认分支（A）：只含 root + A
    let loaded = store.load_branch(&session, &branch.a1).await.unwrap();
    assert_eq!(
        loaded,
        vec![user_message("root", 1_000), user_message("branch A", 2_000)]
    );

    // 默认分支（B 叶子）：root + B + tool
    let loaded = store.load_branch(&session, &branch.tool).await.unwrap();
    assert_eq!(
        loaded,
        vec![
            user_message("root", 1_000),
            user_message("branch B", 3_000),
            tool_result_message(4_000),
        ]
    );

    // 中间节点：路径到该节点为止，不含后代
    let loaded = store.load_branch(&session, &branch.root).await.unwrap();
    assert_eq!(loaded, vec![user_message("root", 1_000)]);
}

#[tokio::test]
async fn load_branch_rejects_foreign_or_missing_entry() {
    let (store, session, branch) = branched_store().await;
    let other = store.create_session("/tmp/q").await.unwrap();

    // 其他 session 的 entry id
    let result = store.load_branch(&other, &branch.a1).await;
    assert!(matches!(result, Err(SessionError::EntryNotFound(_))));

    // 不存在的 entry id
    let result = store.load_branch(&session, "no-such-entry").await;
    assert!(matches!(result, Err(SessionError::EntryNotFound(_))));

    // 不存在的 session
    let result = store.load_branch("no-such-session", &branch.a1).await;
    assert!(matches!(result, Err(SessionError::SessionNotFound(_))));
}

#[tokio::test]
async fn load_branch_replays_compaction_on_path() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    store
        .append_message(&session, None, &user_message("m1", 1_000))
        .await
        .unwrap();
    store
        .append_message(&session, None, &user_message("m2", 2_000))
        .await
        .unwrap();
    let compacted = store
        .append_compaction(&session, None, &compaction("summary of m1+m2", 1))
        .await
        .unwrap();
    // 压缩后分叉：分支 B 从压缩条目续写
    let branch_b = store
        .append_message(&session, Some(&compacted), &user_message("after B", 3_000))
        .await
        .unwrap();

    // 分支路径重放：kept_count 相对路径前缀精确成立（保留 m2）
    let loaded = store.load_branch(&session, &branch_b).await.unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(extract_summary(&loaded[0]), Some("summary of m1+m2"));
    assert_eq!(loaded[1], user_message("m2", 2_000));
    assert_eq!(loaded[2], user_message("after B", 3_000));
}

#[tokio::test]
async fn latest_entry_id_walks_default_branch() {
    let (store, session, branch) = branched_store().await;

    // 默认分支沿最新子节点：root → B → tool（而非全局最新之外的节点）
    let tip = store.latest_entry_id(&session).await.unwrap();
    assert_eq!(tip.as_deref(), Some(branch.tool.as_str()));

    // 分支 A 续写一条（全局最新 entry 落在 A 上）：默认分支 tip 不变
    store
        .append_message(
            &session,
            Some(&branch.a1),
            &user_message("A continued", 5_000),
        )
        .await
        .unwrap();
    let tip = store.latest_entry_id(&session).await.unwrap();
    assert_eq!(tip.as_deref(), Some(branch.tool.as_str()));

    // 空 session：None
    let empty = store.create_session("/tmp/empty").await.unwrap();
    assert_eq!(store.latest_entry_id(&empty).await.unwrap(), None);
}

#[tokio::test]
async fn list_tree_returns_all_entries_with_previews() {
    let (store, session, branch) = branched_store().await;

    let entries = store.list_tree(&session).await.unwrap();
    assert_eq!(entries.len(), 4, "全部 entry（含各分支）按插入序列出");

    let [root, a1, a2, tool] = &entries[..] else {
        panic!("unexpected entries: {entries:?}");
    };
    assert_eq!(root.id, branch.root);
    assert_eq!(root.parent_id, None);
    assert_eq!(root.role, "user");
    assert_eq!(root.preview, "root");
    assert!(root.is_branchable());

    assert_eq!(a1.parent_id.as_deref(), Some(branch.root.as_str()));
    assert_eq!(a2.parent_id.as_deref(), Some(branch.root.as_str()));
    assert_eq!(a1.preview, "branch A");

    assert_eq!(tool.id, branch.tool);
    assert_eq!(tool.parent_id.as_deref(), Some(branch.a2.as_str()));
    assert_eq!(tool.role, "tool_result");
    assert_eq!(tool.preview, "工具结果：read");
    assert!(!tool.is_branchable(), "工具结果条目不可作为分支起点");
}

#[tokio::test]
async fn list_tree_marks_assistant_tool_calls_unbranchable() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    // assistant_message 含 ToolCall 块
    store
        .append_message(&session, None, &assistant_message(1_000))
        .await
        .unwrap();

    let entries = store.list_tree(&session).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].has_tool_calls);
    assert!(
        !entries[0].is_branchable(),
        "含工具调用的 assistant 条目不可作为分支起点（避免悬空 tool_use）"
    );
    // 预览取首个文本块
    assert_eq!(entries[0].preview, "done");
}

#[tokio::test]
async fn list_tree_compaction_and_error_previews() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    store
        .append_message(&session, None, &user_message("hi", 1_000))
        .await
        .unwrap();
    store
        .append_compaction(&session, None, &compaction("summary", 3))
        .await
        .unwrap();
    // 失败的 assistant 响应
    let mut failed = assistant_message(2_000);
    if let Message::Assistant(assistant) = &mut failed {
        assistant.content.clear();
        assistant.stop_reason = StopReason::Error;
        assistant.error_message = Some("rate limited".to_string());
    }
    store.append_message(&session, None, &failed).await.unwrap();

    let entries = store.list_tree(&session).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1].role, "compaction");
    assert_eq!(entries[1].preview, "上下文压缩（保留 3 条近期消息）");
    assert!(entries[1].is_branchable(), "压缩条目可作为分支起点");
    assert_eq!(entries[2].preview, "（响应失败：rate limited）");
}

#[tokio::test]
async fn append_compaction_with_explicit_parent_branches() {
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/p").await.unwrap();
    let root = store
        .append_message(&session, None, &user_message("root", 1_000))
        .await
        .unwrap();
    // 显式 parent 的压缩条目：链到 root 而非全局最新
    let compacted = store
        .append_compaction(&session, Some(&root), &compaction("branch summary", 0))
        .await
        .unwrap();

    let loaded = store.load_branch(&session, &compacted).await.unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(extract_summary(&loaded[0]), Some("branch summary"));

    // 显式 parent 不存在：EntryNotFound
    let result = store
        .append_compaction(&session, Some("no-such-entry"), &compaction("x", 0))
        .await;
    assert!(matches!(result, Err(SessionError::EntryNotFound(_))));
}

// ── config 表：append-only 配置历史与回退读取 ──────────────────────────────

#[tokio::test]
async fn config_history_is_append_only_newest_first() {
    let store = SessionStore::in_memory().await.unwrap();
    assert!(store.config_history("model").await.unwrap().is_empty());

    store
        .set_config("model", &serde_json::json!("anthropic/claude-sonnet-4-5"))
        .await
        .unwrap();
    store
        .set_config("model", &serde_json::json!("openai/gpt-5.2"))
        .await
        .unwrap();
    store
        .set_config("other", &serde_json::json!({"enabled": true}))
        .await
        .unwrap();

    let history = store.config_history("model").await.unwrap();
    assert_eq!(
        history,
        vec![
            serde_json::json!("openai/gpt-5.2"),
            serde_json::json!("anthropic/claude-sonnet-4-5"),
        ],
        "最新在前；不同 key 互不干扰"
    );
}

#[tokio::test]
async fn get_config_falls_back_past_mismatched_rows() {
    let store = SessionStore::in_memory().await.unwrap();
    store
        .set_config("model", &serde_json::json!("openai/gpt-5.2"))
        .await
        .unwrap();
    // 最新一行类型不符（本可能被库外写入/未来 schema 演进产生）：跳过它回退
    store
        .set_config("model", &serde_json::json!({"provider": "openai"}))
        .await
        .unwrap();

    let selected: Option<String> = store.get_config("model").await.unwrap();
    assert_eq!(selected.as_deref(), Some("openai/gpt-5.2"));
}

#[tokio::test]
async fn get_config_returns_none_when_nothing_to_fall_back_to() {
    let store = SessionStore::in_memory().await.unwrap();
    let missing: Option<String> = store.get_config("model").await.unwrap();
    assert_eq!(missing, None);

    store
        .set_config("model", &serde_json::json!(42))
        .await
        .unwrap();
    let mismatched: Option<String> = store.get_config("model").await.unwrap();
    assert_eq!(mismatched, None, "唯一的行类型不符：无可回退");
}

#[tokio::test]
async fn config_records_update_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open(&path).await.unwrap();
    let before = nomic_ai::now_millis();
    store
        .set_config("model", &serde_json::json!("openai/gpt-5.2"))
        .await
        .unwrap();
    let after = nomic_ai::now_millis();

    // updated_at 由写入方记录（Unix 毫秒）；经独立连接直查验证列存在且落值
    let pool = sqlx::SqlitePool::connect(path.to_str().unwrap())
        .await
        .unwrap();
    let updated_at: i64 =
        sqlx::query_scalar("SELECT updated_at FROM config WHERE \"key\" = 'model'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        before.cast_signed() <= updated_at && updated_at <= after.cast_signed(),
        "updated_at {updated_at} 应落在写入前后 [{before}, {after}] 之间"
    );
}
