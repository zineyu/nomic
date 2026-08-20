//! workspace：登记去重、路径规范化、按 workspace 过滤 session、0005 迁移语义。

use std::path::Path;

use nomic_session::SessionStore;

#[tokio::test]
async fn get_or_create_workspace_dedups_by_path() {
    let store = SessionStore::in_memory().await.unwrap();
    let first = store.get_or_create_workspace("/tmp/ws-a").await.unwrap();
    let second = store.get_or_create_workspace("/tmp/ws-a").await.unwrap();
    assert_eq!(first.id, second.id, "同路径复用同一 workspace");

    let listed = store.list_workspaces().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, Path::new("/tmp/ws-a"));
    assert_eq!(listed[0].session_count, 0);
    assert_eq!(
        listed[0].last_active_at, None,
        "无 session 活动时无活跃时间"
    );
}

#[tokio::test]
async fn workspace_path_is_normalized() {
    let store = SessionStore::in_memory().await.unwrap();
    // 相对路径按进程 cwd 解析（canonicalize）
    let workspace = store.get_or_create_workspace(".").await.unwrap();
    assert!(workspace.path.is_absolute(), "{}", workspace.path.display());
    // 不存在的路径退回原始文本（不报错）
    let missing = store
        .get_or_create_workspace("/tmp/nomic-test-nonexistent-ws")
        .await
        .unwrap();
    assert_eq!(missing.path, Path::new("/tmp/nomic-test-nonexistent-ws"));
}

#[tokio::test]
async fn list_sessions_in_filters_by_workspace() {
    let store = SessionStore::in_memory().await.unwrap();
    let a = store.create_session("/tmp/ws-a").await.unwrap();
    let b = store.create_session("/tmp/ws-b").await.unwrap();
    let a2 = store.create_session("/tmp/ws-a").await.unwrap();

    let summaries = store.list_sessions().await.unwrap();
    let ws_a = summaries
        .iter()
        .find(|s| s.id == a)
        .unwrap()
        .workspace_id
        .clone();
    let ws_b = summaries
        .iter()
        .find(|s| s.id == b)
        .unwrap()
        .workspace_id
        .clone();

    let in_a: Vec<String> = store
        .list_sessions_in(&ws_a)
        .await
        .unwrap()
        .iter()
        .map(|s| s.id.clone())
        .collect();
    assert_eq!(in_a.len(), 2);
    assert!(in_a.contains(&a) && in_a.contains(&a2));
    assert_eq!(store.list_sessions_in(&ws_b).await.unwrap().len(), 1);

    let workspace = store.workspace_of_session(&a).await.unwrap().unwrap();
    assert_eq!(workspace.id, ws_a);
    assert_eq!(workspace.path, Path::new("/tmp/ws-a"));
    assert_eq!(
        store.session_workspace_path(&a).await.unwrap(),
        Path::new("/tmp/ws-a")
    );
}

#[tokio::test]
async fn append_message_advances_workspace_activity() {
    use nomic_ai::{Message, UserMessage, UserMessageContent};
    let store = SessionStore::in_memory().await.unwrap();
    let session = store.create_session("/tmp/ws-a").await.unwrap();
    let message = Message::User(UserMessage {
        content: UserMessageContent::Text("hi".to_string()),
        timestamp: 1_000,
    });
    store
        .append_message(&session, None, &message)
        .await
        .unwrap();

    let workspaces = store.list_workspaces().await.unwrap();
    assert_eq!(workspaces[0].last_active_at, Some(1_000));
}

/// 0005 迁移脚本语义：旧库（sessions.cwd）迁移后每个 distinct cwd 登记为
/// workspace，sessions.workspace_id 回填，cwd 列删除。
///
/// 在临时库上依次执行 0001..0005 的 SQL 原文验证（不走 sqlx 迁移记录，
/// 仅验证脚本本身的数据迁移正确性）。
#[tokio::test]
async fn migration_0005_moves_cwd_into_workspaces() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();

    // 依次应用 0001..0004 构造旧库，再执行 0005 迁移脚本
    for sql in [
        include_str!("../migrations/0001_init.sql"),
        include_str!("../migrations/0002_entry_kind.sql"),
        include_str!("../migrations/0003_config.sql"),
        include_str!("../migrations/0004_session_config.sql"),
    ] {
        sqlx::raw_sql(sql).execute(&pool).await.unwrap();
    }
    // 旧 schema 数据：两个 session 同属一个 cwd，另一个属其他目录
    for (id, cwd) in [
        ("s1", "/tmp/proj"),
        ("s2", "/tmp/proj"),
        ("s3", "/tmp/other"),
    ] {
        sqlx::query("INSERT INTO sessions (id, cwd) VALUES (?, ?)")
            .bind(id)
            .bind(cwd)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::raw_sql(include_str!("../migrations/0005_workspaces.sql"))
        .execute(&pool)
        .await
        .unwrap();

    // distinct cwd 各登记一个 workspace
    let workspaces: Vec<(String, String)> =
        sqlx::query_as("SELECT id, path FROM workspaces ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(workspaces.len(), 2);
    assert_eq!(workspaces[0].1, "/tmp/other");
    assert_eq!(workspaces[1].1, "/tmp/proj");

    // sessions.workspace_id 回填：同 cwd 归入同一 workspace
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT s.id, w.path FROM sessions s JOIN workspaces w ON w.id = s.workspace_id ORDER BY s.id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            ("s1".to_string(), "/tmp/proj".to_string()),
            ("s2".to_string(), "/tmp/proj".to_string()),
            ("s3".to_string(), "/tmp/other".to_string()),
        ]
    );

    // cwd 列已删除
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions')")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(!columns.contains(&"cwd".to_string()));
    assert!(columns.contains(&"workspace_id".to_string()));
}
