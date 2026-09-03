//! SQLite 特性保障测试：WAL / 外键约束 / busy_timeout / STRICT 表 / FTS5 / JSON 函数。
//!
//! WAL 与 STRICT 校验库级状态（文件头 / sqlite_master DDL），foreign_keys 与
//! busy_timeout 校验 pool 连接的会话级 pragma，FTS5 与 JSON 验证编译特性可用。

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

use crate::SessionStore;

/// 文件库连接开启 WAL；WAL 是库文件级设置，不经 SessionStore 的新连接同样看到 wal。
#[tokio::test]
async fn file_db_journal_mode_is_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let store = SessionStore::open(&path).await.unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");

    let raw = SqlitePool::connect_with(SqliteConnectOptions::new().filename(&path))
        .await
        .unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&raw)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    raw.close().await;
}

/// 每条 pool 连接都带 foreign_keys=ON 与 5s busy_timeout。
#[tokio::test]
async fn connections_enforce_foreign_keys_with_busy_timeout() {
    let store = SessionStore::in_memory().await.unwrap();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(foreign_keys, 1);
    let busy_timeout_ms: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(busy_timeout_ms, 5000);
}

/// 外键约束行为验证：引用不存在 project 的 session 写入被拒绝。
#[tokio::test]
async fn foreign_key_violation_is_rejected() {
    let store = SessionStore::in_memory().await.unwrap();
    let result = store.create_session_in("no-such-project").await;
    assert!(result.is_err());
}

/// 迁移 0006 后全部业务表为 STRICT 表；STRICT 行为验证：INTEGER 列拒绝
/// 不可无损转换的 TEXT 写入（注意 STRICT 允许 123 → '123' 这类无损转换）。
#[tokio::test]
async fn business_tables_are_strict() {
    let store = SessionStore::in_memory().await.unwrap();
    for table in ["projects", "sessions", "entries", "config"] {
        let ddl: String = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = ?")
            .bind(table)
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert!(
            ddl.contains("STRICT"),
            "{table} 应为 STRICT 表，实际 DDL：{ddl}"
        );
    }

    let rejected =
        sqlx::query("INSERT INTO sessions (id, first_message_at) VALUES ('s1', 'not-a-number')")
            .execute(&store.pool)
            .await;
    assert!(
        rejected.is_err(),
        "STRICT 表 INTEGER 列必须拒绝不可转换的 TEXT 写入"
    );
}

/// bundled SQLite 编译带 FTS5：建虚表、写入、MATCH 查询全链路可用。
#[tokio::test]
async fn fts5_is_available() {
    let store = SessionStore::in_memory().await.unwrap();
    sqlx::query("CREATE VIRTUAL TABLE fts_probe USING fts5(body)")
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO fts_probe (body) VALUES ('hello sqlite world')")
        .execute(&store.pool)
        .await
        .unwrap();
    let hits: i64 =
        sqlx::query_scalar("SELECT count(*) FROM fts_probe WHERE fts_probe MATCH 'world'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(hits, 1);
}

/// JSON 函数内置可用；jsonb 二进制格式（config 表 value 列的存储格式）往返保真。
#[tokio::test]
async fn json_functions_are_available() {
    let store = SessionStore::in_memory().await.unwrap();
    let answer: i64 = sqlx::query_scalar(r#"SELECT json_extract('{"a": 41}', '$.a') + 1"#)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(answer, 42);
    let roundtrip: String = sqlx::query_scalar(r#"SELECT json(jsonb('{"k": "v"}'))"#)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(roundtrip, r#"{"k":"v"}"#);
}
