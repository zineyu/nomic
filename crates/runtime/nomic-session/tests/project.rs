//! project：登记去重、路径规范化、按 project 过滤 session、0005 迁移语义。

use std::path::{Path, PathBuf};

use nomic_session::SessionStore;

#[tokio::test]
async fn get_or_create_project_dedups_by_path() {
    let store = SessionStore::in_memory().await.unwrap();
    let first = store.get_or_create_project("/tmp/ws-a").await.unwrap();
    let second = store.get_or_create_project("/tmp/ws-a").await.unwrap();
    assert_eq!(first.id, second.id, "同路径复用同一 project");

    let listed = store.list_projects().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, Path::new("/tmp/ws-a"));
    assert_eq!(listed[0].session_count, 0);
    assert_eq!(
        listed[0].last_active_at, None,
        "无 session 活动时无活跃时间"
    );
}

#[tokio::test]
async fn project_path_is_normalized() {
    let store = SessionStore::in_memory().await.unwrap();
    // 相对路径按进程 cwd 解析（canonicalize）
    let project = store.get_or_create_project(".").await.unwrap();
    assert!(project.path.is_absolute(), "{}", project.path.display());
    // 不存在的路径退回原始文本（不报错）
    let missing = store
        .get_or_create_project("/tmp/nomic-test-nonexistent-ws")
        .await
        .unwrap();
    assert_eq!(missing.path, Path::new("/tmp/nomic-test-nonexistent-ws"));
}

#[tokio::test]
async fn list_sessions_in_filters_by_project() {
    use nomic_ai::{Message, UserMessage, UserMessageContent};
    let store = SessionStore::in_memory().await.unwrap();
    let a = store.create_session("/tmp/ws-a").await.unwrap();
    let b = store.create_session("/tmp/ws-b").await.unwrap();
    let a2 = store.create_session("/tmp/ws-a").await.unwrap();

    // 列表只包含有 user 消息的 session（空壳不进列表口径）
    for id in [&a, &b, &a2] {
        let message = Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_string()),
            timestamp: 1_000,
        });
        store.append_message(id, None, &message).await.unwrap();
    }

    let summaries = store.list_sessions().await.unwrap();
    let ws_a = summaries
        .iter()
        .find(|s| s.id == a)
        .unwrap()
        .project_id
        .clone();
    let ws_b = summaries
        .iter()
        .find(|s| s.id == b)
        .unwrap()
        .project_id
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

    let project = store.project_of_session(&a).await.unwrap().unwrap();
    assert_eq!(project.id, ws_a);
    assert_eq!(project.path, Path::new("/tmp/ws-a"));
    assert_eq!(
        store.session_project_path(&a).await.unwrap(),
        Path::new("/tmp/ws-a")
    );
}

#[tokio::test]
async fn append_message_advances_project_activity() {
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

    let projects = store.list_projects().await.unwrap();
    assert_eq!(projects[0].last_active_at, Some(1_000));
}

#[tokio::test]
async fn list_projects_keeps_registration_order_despite_activity() {
    use nomic_ai::{Message, UserMessage, UserMessageContent};
    let store = SessionStore::in_memory().await.unwrap();
    store.get_or_create_project("/tmp/ws-a").await.unwrap();
    let b = store.create_session("/tmp/ws-b").await.unwrap();
    // ws-b 产生活动（推进 last_active_at），但列表顺序不随活跃度浮动
    let message = Message::User(UserMessage {
        content: UserMessageContent::Text("hi".to_string()),
        timestamp: 1_000,
    });
    store.append_message(&b, None, &message).await.unwrap();

    let projects = store.list_projects().await.unwrap();
    assert_eq!(
        projects.iter().map(|w| w.path.clone()).collect::<Vec<_>>(),
        vec![PathBuf::from("/tmp/ws-a"), PathBuf::from("/tmp/ws-b")],
        "列表顺序以登记时间为准，活跃度不置顶",
    );
}

/// 0005 迁移脚本语义：旧库（sessions.cwd）迁移后每个 distinct cwd 登记为
/// workspace，sessions.workspace_id 回填，cwd 列删除；再经 0009 改名为
/// projects / project_id。
///
/// 在临时库上依次执行 0001..0005 与 0009 的 SQL 原文验证（不走 sqlx 迁移
/// 记录，仅验证脚本本身的数据迁移正确性）。
#[tokio::test]
async fn migration_0005_moves_cwd_into_projects() {
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
    // 0009 改名：workspaces → projects，sessions.workspace_id → project_id
    sqlx::raw_sql(include_str!("../migrations/0009_rename_projects.sql"))
        .execute(&pool)
        .await
        .unwrap();

    // distinct cwd 各登记一个 project
    let projects: Vec<(String, String)> =
        sqlx::query_as("SELECT id, path FROM projects ORDER BY path")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(projects.len(), 2);
    assert_eq!(projects[0].1, "/tmp/other");
    assert_eq!(projects[1].1, "/tmp/proj");

    // sessions.project_id 回填：同 cwd 归入同一 project
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT s.id, w.path FROM sessions s JOIN projects w ON w.id = s.project_id ORDER BY s.id",
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
    assert!(columns.contains(&"project_id".to_string()));
}
