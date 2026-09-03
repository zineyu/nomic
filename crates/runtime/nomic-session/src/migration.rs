//! entries payload 的一次性数据迁移：旧双格式（`Message` /
//! `CompactionRecord` JSON）→ 统一 [`Entry`] JSON（ADR-0045）。
//!
//! payload 重写无法用纯 SQL 表达（字段重组依赖 serde 类型的格式定义），
//! 因此由本模块在 Rust 侧执行，以 `PRAGMA user_version` 把关（0 → 1；
//! sqlx 自身不使用该 pragma）。**执行顺序**：本迁移先于 sqlx 迁移运行
//! （`SessionStore::migrate`），因为 migration 0013 的 parts 表拆解 SQL
//! 只认 Entry 格式；全新库（entries 表尚不存在）直接跳过，新库写入
//! 天然是新格式。判别是纯结构性的：含 `parts` 字段即新格式（跳过，幂等）；
//! 含 `role` 为旧 `Message`；含 `summary` 为旧 `CompactionRecord`。
//! 解析失败的行保留原样并告警（加载行为与迁移前一致：该 entry 报错，
//! 不阻断整体）。

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

/// 执行 payload 数据迁移（幂等）：版本达标或 entries 表尚未建立
/// （全新库）时直接返回。
pub async fn migrate_entry_payloads(pool: &SqlitePool) -> Result<(), SessionError> {
    let table_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'entries')",
    )
    .fetch_one(pool)
    .await?;
    if !table_exists {
        return Ok(());
    }
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

    /// 旧格式行：(id, parent_id, role, kind, timestamp, payload)
    type LegacyRow = (
        &'static str,
        Option<&'static str>,
        &'static str,
        &'static str,
        i64,
        &'static str,
    );

    /// 构造「旧格式库」：按 0001..0011 的 SQL 原文建 schema（不走 sqlx
    /// 迁移记录；payload 列与 kind 列仍在），插入旧双格式 payload（外加
    /// 一条损坏 payload 验证 json_valid 守卫）。
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
             VALUES ('s1', 'w-s1', 1000, 6000)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let legacy: [LegacyRow; 6] = [
            (
                "e1",
                None,
                "user",
                "message",
                1_000,
                r#"{"role":"user","content":"第一条","timestamp":1000}"#,
            ),
            (
                "e2",
                Some("e1"),
                "assistant",
                "message",
                2_000,
                r#"{"role":"assistant","content":[{"type":"thinking","thinking":"想一下","thinking_signature":"sig"},{"type":"text","text":"回答一"},{"type":"tool_call","id":"call-1","name":"read","arguments":{"path":"a.rs"}}],"api":"anthropic_messages","provider":"anthropic","model":"claude","usage":{"input":1,"output":2,"cache_read":0,"cache_write":0,"total_tokens":3,"cost":{"input":0.0,"output":0.0,"cache_read":0.0,"cache_write":0.0,"total":0.0}},"stop_reason":"tool_use","timestamp":2000}"#,
            ),
            (
                "e3",
                Some("e2"),
                "tool_result",
                "message",
                3_000,
                r#"{"role":"tool_result","tool_call_id":"call-1","tool_name":"read","content":[{"type":"text","text":"文件内容"},{"type":"image","data":"aW1n","mime_type":"image/png"}],"is_error":false,"timestamp":3000}"#,
            ),
            (
                "e4",
                Some("e3"),
                "user",
                "message",
                4_000,
                r#"{"role":"user","content":[{"type":"text","text":"第二条"},{"type":"image","data":"aW1n","mime_type":"image/png"}],"timestamp":4000}"#,
            ),
            (
                "e5",
                Some("e4"),
                "compaction",
                "compaction",
                5_000,
                r#"{"summary":"前四条摘要","kept_count":2,"tokens_before":999}"#,
            ),
            ("e6", Some("e5"), "user", "message", 6_000, "not json"),
        ];
        for (id, parent, role, kind, ts, payload) in &legacy {
            sqlx::query(
                "INSERT INTO entries (id, session_id, parent_id, role, timestamp, payload, kind)
                 VALUES (?, 's1', ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(parent)
            .bind(role)
            .bind(ts)
            .bind(payload)
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        (dir, pool)
    }

    /// 走完新代码的完整迁移管线：Rust 旧格式转换（v0→v1）+ SQL 迁移
    /// 0012（删 kind）与 0013（拆 parts 表）。
    async fn run_full_migration(pool: &SqlitePool) {
        migrate_entry_payloads(pool).await.unwrap();
        for sql in [
            include_str!("../migrations/0012_drop_entry_kind.sql"),
            include_str!("../migrations/0013_entry_parts.sql"),
        ] {
            sqlx::raw_sql(sql).execute(pool).await.unwrap();
        }
    }

    /// 旧 payload 经管线后：parts 行按类型各归其列（含 thinking 签名、
    /// 工具调用参数、tool_result 嵌套块），meta 列承载 assistant 响应
    /// 元数据，payload/kind 列删除；损坏 payload 行降级为空 parts 条目。
    // 逐类断言是测试本体，拆开反而割裂语境
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn legacy_payloads_decompose_into_parts_table() {
        let (_dir, pool) = legacy_db().await;
        run_full_migration(&pool).await;

        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, ENTRY_PAYLOAD_VERSION);

        // entries 表形态：无 payload / kind，有 meta
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('entries')")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(!columns.contains(&"payload".to_string()));
        assert!(!columns.contains(&"kind".to_string()));
        assert!(columns.contains(&"meta".to_string()));

        // e2（assistant）：thinking/text/tool_call 三块，签名与参数落列
        let rows = sqlx::query(
            "SELECT seq, type, text, signature, tool_name, arguments FROM parts
             WHERE entry_id = 'e2' ORDER BY seq, sub_seq",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get::<&str, _>("type"), "thinking");
        assert_eq!(rows[0].get::<Option<&str>, _>("signature"), Some("sig"));
        assert_eq!(rows[1].get::<Option<&str>, _>("text"), Some("回答一"));
        assert_eq!(rows[2].get::<&str, _>("type"), "tool_call");
        assert_eq!(rows[2].get::<Option<&str>, _>("tool_name"), Some("read"));
        assert_eq!(
            rows[2].get::<Option<&str>, _>("arguments"),
            Some(r#"{"path":"a.rs"}"#)
        );
        let meta: String = sqlx::query_scalar("SELECT meta FROM entries WHERE id = 'e2'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let response: nomic_ai::ResponseMeta = serde_json::from_str(&meta).unwrap();
        assert_eq!(response.model, "claude");
        assert_eq!(response.stop_reason, nomic_ai::StopReason::ToolUse);

        // e3（tool_result）：顶层块 sub_seq=0，两个嵌套内容块 sub_seq=1/2
        let rows = sqlx::query(
            "SELECT sub_seq, type, text, mime_type FROM parts
             WHERE entry_id = 'e3' ORDER BY sub_seq",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get::<&str, _>("type"), "tool_result");
        assert_eq!(rows[1].get::<Option<&str>, _>("text"), Some("文件内容"));
        assert_eq!(
            rows[2].get::<Option<&str>, _>("mime_type"),
            Some("image/png")
        );

        // e5（compaction）：记录落列
        let (summary, kept, tokens): (String, i64, i64) = sqlx::query_as(
            "SELECT summary, kept_count, tokens_before FROM parts WHERE entry_id = 'e5'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((summary.as_str(), kept, tokens), ("前四条摘要", 2, 999));

        // e6（损坏 payload）：json_valid 守卫跳过，降级为空 parts 条目
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM parts WHERE entry_id = 'e6'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    /// 迁移后重放语义逐字节不变：kept_count=2 → 摘要 + e3/e4（压缩前的
    /// 有效尾部）；损坏的 e6 降级为空内容的 user 消息。
    #[tokio::test]
    async fn replay_semantics_survive_migration() {
        let (_dir, pool) = legacy_db().await;
        run_full_migration(&pool).await;

        let store = SessionStore { pool };
        let loaded = store.load_branch("s1", "e6").await.unwrap();
        let texts: Vec<String> = loaded
            .iter()
            .map(|message| match message {
                Message::User(user) => match &user.content {
                    nomic_ai::UserMessageContent::Text(text) => text.clone(),
                    nomic_ai::UserMessageContent::Blocks(blocks) => {
                        format!("blocks×{}", blocks.len())
                    }
                },
                Message::Assistant(_) => "assistant".to_string(),
                Message::ToolResult(result) => format!("tool_result:{}", result.tool_name),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "The conversation history before this point was compacted into the following summary:\n<summary>\n前四条摘要\n</summary>",
                "tool_result:read",
                "blocks×2",
                "blocks×0",
            ]
        );
    }

    /// 新库（无任何旧数据）迁移为空转，user_version 照常推进。
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
