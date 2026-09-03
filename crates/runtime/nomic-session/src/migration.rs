//! entries payload 的一次性数据迁移：旧双格式（`Message` /
//! `CompactionRecord` JSON）→ 统一 [`Entry`] JSON（ADR-0045）。
//!
//! payload 重写无法用纯 SQL 表达（字段重组依赖 serde 类型的格式定义），
//! 因此在 sqlx 迁移之后由本模块在 Rust 侧执行，以 `PRAGMA user_version`
//! 把关（0 → 1；sqlx 自身不使用该 pragma）。判别是纯结构性的：
//! 含 `parts` 字段即新格式（跳过，幂等）；含 `role` 为旧 `Message`；
//! 含 `summary` 为旧 `CompactionRecord`。解析失败的行保留原样并告警
//! （加载行为与迁移前一致：该 entry 报错，不阻断整体）。

use nomic_ai::{CompactionRecord, Entry, Message};
use sqlx::{Row as _, SqlitePool};

use crate::{SessionError, to_u64};

/// entries payload 格式版本（`PRAGMA user_version`）。
const ENTRY_PAYLOAD_VERSION: i64 = 1;

/// 旧格式 payload 的迁移结果。
enum PayloadMigration {
    /// 已是新格式（含 `parts` 字段），跳过
    Current,
    /// 转换成功，新 payload
    Converted(String),
    /// 无法解析（数据损坏），保留原样
    Unparsable,
}

/// 执行 payload 数据迁移（幂等）：版本达标时直接返回。
pub async fn migrate_entry_payloads(pool: &SqlitePool) -> Result<(), SessionError> {
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await?;
    if version >= ENTRY_PAYLOAD_VERSION {
        return Ok(());
    }

    // 与 append_entry 同理预先取写锁，避免 WAL 下读写升级竞争
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let rows = sqlx::query("SELECT id, timestamp, payload FROM entries ORDER BY rowid")
        .fetch_all(&mut *tx)
        .await?;

    let (mut migrated, mut current, mut unparsable) = (0_u64, 0_u64, 0_u64);
    for row in &rows {
        let payload: String = row.get("payload");
        match convert_legacy_payload(&payload, to_u64(row.get("timestamp"))) {
            PayloadMigration::Current => current += 1,
            PayloadMigration::Unparsable => {
                unparsable += 1;
                tracing::warn!(entry_id = %row.get::<String, _>("id"), "entry payload 无法解析，保留原样");
            }
            PayloadMigration::Converted(new_payload) => {
                sqlx::query("UPDATE entries SET payload = ? WHERE id = ?")
                    .bind(new_payload)
                    .bind(row.get::<String, _>("id"))
                    .execute(&mut *tx)
                    .await?;
                migrated += 1;
            }
        }
    }

    // PRAGMA 不支持参数绑定；版本号本身即为编译期常量，直接写字面量
    sqlx::query("PRAGMA user_version = 1")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    tracing::info!(
        migrated,
        current,
        unparsable,
        "entries payload 迁移完成（统一 Entry 格式）"
    );
    Ok(())
}

/// 单条 payload 的格式判别与转换；`timestamp` 取自行列（旧
/// `CompactionRecord` payload 不含时间戳，迁移时从提取列回填）。
fn convert_legacy_payload(payload: &str, timestamp: u64) -> PayloadMigration {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return PayloadMigration::Unparsable;
    };
    let Some(object) = value.as_object() else {
        return PayloadMigration::Unparsable;
    };
    if object.contains_key("parts") {
        return PayloadMigration::Current;
    }
    let entry = if object.contains_key("role") {
        match serde_json::from_value::<Message>(value) {
            Ok(message) => Entry::from(&message),
            Err(_) => return PayloadMigration::Unparsable,
        }
    } else if object.contains_key("summary") {
        match serde_json::from_value::<CompactionRecord>(value) {
            Ok(record) => Entry::compaction(record, timestamp),
            Err(_) => return PayloadMigration::Unparsable,
        }
    } else {
        return PayloadMigration::Unparsable;
    };
    match serde_json::to_string(&entry) {
        Ok(payload) => PayloadMigration::Converted(payload),
        Err(_) => PayloadMigration::Unparsable,
    }
}

#[cfg(test)]
mod tests {
    use sqlx::Row as _;
    use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};

    use super::*;
    use crate::SessionStore;

    /// 构造「旧格式库」：按 0001..0012 的 SQL 原文建 schema（不走 sqlx
    /// 迁移记录），插入旧双格式 payload（外加一条新格式 entry 验证幂等
    /// 跳过、一条损坏 payload 验证保留原样）。
    // 建库 + 插数据本质是声明式脚本，拆开反而打断阅读
    #[allow(clippy::too_many_lines)]
    async fn legacy_db() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();

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
            include_str!("../migrations/0010_works.sql"),
            include_str!("../migrations/0011_sessions_work_index.sql"),
            include_str!("../migrations/0012_drop_entry_kind.sql"),
        ] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }

        // 旧 schema 归属链：project → work → session
        sqlx::query(
            "INSERT INTO projects (id, path, created_at, last_active_at)
             VALUES ('p1', '/tmp/proj', 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO works (id, project_id, created_at, last_active_at)
             VALUES ('w-s1', 'p1', 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO sessions (id, work_id, first_message_at, last_message_at)
             VALUES ('s1', 'w-s1', 1000, 8000)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let legacy: [(&str, Option<&str>, &str, i64, &str); 8] = [
            (
                "e1",
                None,
                "user",
                1_000,
                r#"{"role":"user","content":"第一条","timestamp":1000}"#,
            ),
            (
                "e2",
                Some("e1"),
                "assistant",
                2_000,
                r#"{"role":"assistant","content":[{"type":"text","text":"回答一"}],"api":"anthropic_messages","provider":"anthropic","model":"claude","usage":{"input":1,"output":2,"cache_read":0,"cache_write":0,"total_tokens":3,"cost":{"input":0.0,"output":0.0,"cache_read":0.0,"cache_write":0.0,"total":0.0}},"stop_reason":"stop","timestamp":2000}"#,
            ),
            (
                "e3",
                Some("e2"),
                "user",
                3_000,
                r#"{"role":"user","content":"第二条","timestamp":3000}"#,
            ),
            (
                "e4",
                Some("e3"),
                "user",
                4_000,
                r#"{"role":"user","content":"第三条","timestamp":4000}"#,
            ),
            (
                "e5",
                Some("e4"),
                "compaction",
                5_000,
                r#"{"summary":"前两条摘要","kept_count":2,"tokens_before":999}"#,
            ),
            (
                "e6",
                Some("e5"),
                "user",
                6_000,
                r#"{"role":"user","content":"第四条","timestamp":6000}"#,
            ),
            (
                "e7",
                Some("e6"),
                "user",
                7_000,
                r#"{"role":"user","parts":[{"type":"text","text":"已是新格式"}],"timestamp":7000}"#,
            ),
            ("e8", Some("e7"), "user", 8_000, "not json"),
        ];
        for (id, parent, role, ts, payload) in &legacy {
            sqlx::query(
                "INSERT INTO entries (id, session_id, parent_id, role, timestamp, payload)
                 VALUES (?, 's1', ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(parent)
            .bind(role)
            .bind(ts)
            .bind(payload)
            .execute(&pool)
            .await
            .unwrap();
        }
        (dir, pool)
    }

    /// 旧 payload 全部重写为统一 Entry 格式；新格式与损坏行原样保留；
    /// user_version 推进且重复执行为 no-op（幂等）。
    #[tokio::test]
    async fn legacy_payloads_are_rewritten_to_entries() {
        let (_dir, pool) = legacy_db().await;
        migrate_entry_payloads(&pool).await.unwrap();

        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, ENTRY_PAYLOAD_VERSION);
        migrate_entry_payloads(&pool).await.unwrap();

        let rows = sqlx::query("SELECT id, payload FROM entries ORDER BY rowid")
            .fetch_all(&pool)
            .await
            .unwrap();
        for row in &rows {
            let id: String = row.get("id");
            let payload: String = row.get("payload");
            if id == "e8" {
                assert_eq!(payload, "not json", "损坏 payload 应原样保留");
                continue;
            }
            let entry: Entry = serde_json::from_str(&payload)
                .unwrap_or_else(|e| panic!("{id} 应为 Entry 格式：{e}"));
            let expected_role = match id.as_str() {
                "e5" => "compaction",
                "e2" => "assistant",
                _ => "user",
            };
            assert_eq!(entry.role.as_str(), expected_role);
        }

        // compaction 记录内容完整（时间戳从行列回填）
        let payload: String = sqlx::query_scalar("SELECT payload FROM entries WHERE id = 'e5'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let entry: Entry = serde_json::from_str(&payload).unwrap();
        let record = entry.compaction_record().unwrap();
        assert_eq!(record.summary, "前两条摘要");
        assert_eq!(record.kept_count, 2);
        assert_eq!(record.tokens_before, 999);
        assert_eq!(entry.timestamp, 5_000);
    }

    /// 迁移后重放语义逐字节不变：kept_count=2 → 摘要 + e3/e4（压缩前的
    /// 有效尾部）+ e6 + e7（损坏的 e8 使完整加载报错，逐段验证到 e7）。
    #[tokio::test]
    async fn replay_semantics_survive_migration() {
        let (_dir, pool) = legacy_db().await;
        migrate_entry_payloads(&pool).await.unwrap();

        let store = SessionStore { pool };
        let loaded = store.load_branch("s1", "e7").await.unwrap();
        let texts: Vec<String> = loaded
            .iter()
            .map(|message| match message {
                Message::User(user) => match &user.content {
                    nomic_ai::UserMessageContent::Text(text) => text.clone(),
                    nomic_ai::UserMessageContent::Blocks(_) => "blocks".to_string(),
                },
                Message::Assistant(_) => "assistant".to_string(),
                Message::ToolResult(_) => "tool_result".to_string(),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "The conversation history before this point was compacted into the following summary:\n<summary>\n前两条摘要\n</summary>",
                "第二条",
                "第三条",
                "第四条",
                "已是新格式",
            ]
        );
    }

    /// 新库（无任何旧数据）迁移为 no-op，user_version 照常推进。
    #[tokio::test]
    async fn fresh_store_marks_payload_version() {
        let store = SessionStore::in_memory().await.unwrap();
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(version, ENTRY_PAYLOAD_VERSION);
    }
}
