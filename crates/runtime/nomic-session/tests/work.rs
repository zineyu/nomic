//! work 实体：创建（连带主 session）、列表口径（空壳过滤、派生标题）、
//! 重命名、级联删除、空壳清理，以及 0010 迁移的 1:1 回填语义。

use nomic_ai::{Message, UserMessage, UserMessageContent};
use nomic_session::{SessionError, SessionStore};

/// 0010 回填断言行：(work id, project id, title, created_at, last_active_at)
type WorkRow = (String, String, Option<String>, i64, Option<i64>);

async fn store() -> SessionStore {
    SessionStore::in_memory().await.expect("in-memory store")
}

fn user_message(text: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp,
    })
}

#[tokio::test]
async fn create_work_creates_main_session_and_links() {
    let store = store().await;
    let created = store.create_work("/tmp/w-proj").await.expect("create work");

    let work = store
        .work(&created.work_id)
        .await
        .expect("query")
        .expect("work row");
    assert_eq!(work.title, None);

    // 主 session 归属该 work，work 归属路径对应 project
    let of_session = store
        .work_of_session(&created.session_id)
        .await
        .expect("query")
        .expect("work of session");
    assert_eq!(of_session.id, created.work_id);
    assert_eq!(
        store
            .main_session_of_work(&created.work_id)
            .await
            .expect("query")
            .as_deref(),
        Some(created.session_id.as_str())
    );
    assert_eq!(
        store
            .session_project_path(&created.session_id)
            .await
            .expect("path"),
        std::path::Path::new("/tmp/w-proj")
    );
}

#[tokio::test]
async fn list_works_filters_empty_and_derives_title_from_main_session() {
    let store = store().await;
    // 空壳 work（无 user 消息）不进列表
    let _empty = store.create_work("/tmp/w-empty").await.expect("empty work");

    let created = store.create_work("/tmp/w-proj").await.expect("work");
    store
        .append_message(
            &created.session_id,
            None,
            &user_message("实现 work 实体", 1_000),
        )
        .await
        .expect("append");

    let works = store.list_works().await.expect("list");
    assert_eq!(works.len(), 1, "空壳 work 不应出现");
    let summary = &works[0];
    assert_eq!(summary.id, created.work_id);
    assert_eq!(summary.main_session_id, created.session_id);
    assert_eq!(
        summary.title.as_deref(),
        Some("实现 work 实体"),
        "缺省标题派生自主 session 首条 user 消息"
    );
    assert_eq!(summary.session_count, 1);
    assert_eq!(summary.message_count, 1);

    // work 自定义标题优先于派生
    store
        .rename_work(&created.work_id, "自定义名")
        .await
        .expect("rename");
    let works = store.list_works().await.expect("list");
    assert_eq!(works[0].title.as_deref(), Some("自定义名"));
    // 清除自定义回退派生
    let cleared = store
        .rename_work(&created.work_id, "  ")
        .await
        .expect("clear");
    assert_eq!(cleared, None);
    let works = store.list_works().await.expect("list");
    assert_eq!(works[0].title.as_deref(), Some("实现 work 实体"));
}

#[tokio::test]
async fn list_works_aggregates_across_sessions() {
    let store = store().await;
    // work a：主 session + 子 agent session 各一条消息，跨 session 聚合
    let a = store.create_work("/tmp/w-agg").await.expect("work a");
    store
        .append_message(&a.session_id, None, &user_message("主会话消息", 1_000))
        .await
        .expect("append main");
    let child = store
        .create_session_in_work(&a.work_id, Some(&a.session_id))
        .await
        .expect("child session");
    store
        .append_message(&child, None, &user_message("子 agent 消息", 2_000))
        .await
        .expect("append child");

    // work b：消息更早，排序应靠后
    let b = store.create_work("/tmp/w-agg").await.expect("work b");
    store
        .append_message(&b.session_id, None, &user_message("更早的 work", 500))
        .await
        .expect("append b");

    let works = store.list_works().await.expect("list");
    assert_eq!(works.len(), 2);
    let first = &works[0];
    assert_eq!(first.id, a.work_id, "按末条消息时间降序");
    assert_eq!(first.session_count, 2, "含子 agent session");
    assert_eq!(first.message_count, 2, "跨 session 汇总消息数");
    assert_eq!(first.first_message_at, Some(1_000));
    assert_eq!(first.last_message_at, Some(2_000));
    assert_eq!(first.main_session_id, a.session_id);
    assert_eq!(works[1].id, b.work_id);
}

#[tokio::test]
async fn delete_work_cascades_sessions_and_entries() {
    let store = store().await;
    let created = store.create_work("/tmp/w-proj").await.expect("work");
    store
        .append_message(&created.session_id, None, &user_message("hi", 1_000))
        .await
        .expect("append");

    let removed = store.delete_work(&created.work_id).await.expect("delete");
    assert_eq!(removed, vec![created.session_id.clone()]);
    assert!(matches!(
        store.load_messages(&created.session_id).await,
        Err(SessionError::SessionNotFound(_))
    ));
    assert!(store.list_works().await.expect("list").is_empty());

    assert!(matches!(
        store.delete_work(&created.work_id).await,
        Err(SessionError::WorkNotFound(_))
    ));
}

#[tokio::test]
async fn empty_work_pruned_with_empty_main_session() {
    let store = store().await;
    let created = store.create_work("/tmp/w-proj").await.expect("work");

    // 主 session 空壳清理连带清除空壳 work
    assert!(
        store
            .delete_if_no_user_message(&created.session_id)
            .await
            .expect("delete empty")
    );
    assert!(
        store.work(&created.work_id).await.expect("query").is_none(),
        "空壳 work 应连带清除"
    );

    // 有 user 消息的 session 不受影响
    let created = store.create_work("/tmp/w-proj").await.expect("work");
    store
        .append_message(&created.session_id, None, &user_message("hi", 1_000))
        .await
        .expect("append");
    assert!(
        !store
            .delete_if_no_user_message(&created.session_id)
            .await
            .expect("no-op")
    );
    assert!(store.work(&created.work_id).await.expect("query").is_some());
}

#[tokio::test]
async fn list_works_in_filters_by_project() {
    let store = store().await;
    let a = store.create_work("/tmp/w-a").await.expect("a");
    let b = store.create_work("/tmp/w-b").await.expect("b");
    for (created, text) in [(&a, "from a"), (&b, "from b")] {
        store
            .append_message(&created.session_id, None, &user_message(text, 1_000))
            .await
            .expect("append");
    }
    let project_b = store
        .project_by_path("/tmp/w-b")
        .await
        .expect("query")
        .expect("project b");
    let works = store.list_works_in(&project_b.id).await.expect("list");
    assert_eq!(works.len(), 1);
    assert_eq!(works[0].id, b.work_id);
}

/// 0010 迁移回填：旧库（0009 之后、0010 之前）的每个 session 生成 1:1
/// work（id 为 'w-' + session id），sessions.work_id 回填，project_id 移除。
///
/// 在临时库上依次执行 0001..0005、0009 与 0010 的 SQL 原文验证（不走
/// sqlx 迁移记录，仅验证脚本本身的数据迁移正确性）。
#[tokio::test]
async fn migration_0010_backfills_one_work_per_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("old.db");
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .expect("connect");

    for sql in [
        include_str!("../migrations/0001_init.sql"),
        include_str!("../migrations/0002_entry_kind.sql"),
        include_str!("../migrations/0003_config.sql"),
        include_str!("../migrations/0004_session_config.sql"),
        include_str!("../migrations/0005_workspaces.sql"),
        include_str!("../migrations/0006_strict_tables.sql"),
        include_str!("../migrations/0007_session_title.sql"),
        include_str!("../migrations/0008_providers_settings.sql"),
        include_str!("../migrations/0009_rename_projects.sql"),
    ] {
        sqlx::raw_sql(sql)
            .execute(&pool)
            .await
            .expect("migrate 0001..0009");
    }
    // 旧 schema 数据：同一 project 下两个 session
    sqlx::query("INSERT INTO projects (id, path, created_at) VALUES ('p1', '/tmp/proj', 1)")
        .execute(&pool)
        .await
        .expect("insert project");
    for (id, title) in [("s1", "任务甲"), ("s2", "任务乙")] {
        sqlx::query(
            "INSERT INTO sessions (id, project_id, title, first_message_at, last_message_at)
             VALUES (?, 'p1', ?, 100, 200)",
        )
        .bind(id)
        .bind(title)
        .execute(&pool)
        .await
        .expect("insert session");
    }

    sqlx::raw_sql(include_str!("../migrations/0010_works.sql"))
        .execute(&pool)
        .await
        .expect("apply 0010");

    // 每个 session 一个 work：id 确定性（'w-' + session id），title/时间戳继承
    let works: Vec<WorkRow> = sqlx::query_as(
        "SELECT id, project_id, title, created_at, last_active_at FROM works ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("query works");
    assert_eq!(
        works,
        vec![
            (
                "w-s1".to_string(),
                "p1".to_string(),
                Some("任务甲".to_string()),
                100,
                Some(200)
            ),
            (
                "w-s2".to_string(),
                "p1".to_string(),
                Some("任务乙".to_string()),
                100,
                Some(200)
            ),
        ]
    );

    // sessions.work_id 回填，project_id 列已移除
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, work_id FROM sessions ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("query sessions");
    assert_eq!(
        rows,
        vec![
            ("s1".to_string(), "w-s1".to_string()),
            ("s2".to_string(), "w-s2".to_string()),
        ]
    );
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('sessions') ORDER BY name")
            .fetch_all(&pool)
            .await
            .expect("columns");
    assert!(!columns.iter().any(|c| c == "project_id"), "{columns:?}");
    assert!(
        columns.iter().any(|c| c == "parent_session_id"),
        "{columns:?}"
    );

    // 迁移后的库可被 SessionStore 正常打开使用（幂等：迁移记录之外重放
    // 0010 不适用，故直接验证数据可读——经 session 反查 work 与 project）
    let project_path: String = sqlx::query_scalar(
        "SELECT p.path FROM projects p
         JOIN works w ON w.project_id = p.id
         JOIN sessions s ON s.work_id = w.id WHERE s.id = 's1'",
    )
    .fetch_one(&pool)
    .await
    .expect("join path");
    assert_eq!(project_path, "/tmp/proj");
}

/// 子 session 血缘（ADR-0044）：`create_session_in_work` 记
/// `parent_session_id`，`session_membership` 返回 work 归属与血缘；主
/// session 血缘为 None。
#[tokio::test]
async fn child_session_lineage_and_membership() {
    let store = store().await;
    let created = store.create_work("/tmp/lineage").await.expect("work");
    let child = store
        .create_session_in_work(&created.work_id, Some(&created.session_id))
        .await
        .expect("child session");

    let (work_id, parent) = store
        .session_membership(&child)
        .await
        .expect("membership")
        .expect("child exists");
    assert_eq!(work_id, created.work_id);
    assert_eq!(parent.as_deref(), Some(created.session_id.as_str()));

    let (main_work, main_parent) = store
        .session_membership(&created.session_id)
        .await
        .expect("membership")
        .expect("main exists");
    assert_eq!(main_work, created.work_id);
    assert_eq!(main_parent, None);

    assert!(
        store
            .session_membership("s-nonexistent")
            .await
            .expect("membership")
            .is_none()
    );

    // 注：list_sessions_in_work 过滤无 user 消息的空壳 session，刚创建的
    // 子 session 不在其中——血缘以 session_membership / 表数据为准
}
