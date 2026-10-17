//! SQLite schema 版本迁移。
//!
//! schema 的全部知识集中在 [`MIGRATIONS`]：每条迁移一个版本号与
//! DDL，打开时按序应用缺失的迁移，全部迁移在一个事务内完成。
//! `user_version` pragma 记录当前版本；高于本实现版本的库拒绝打开。
//!
//! 测试 fixture（`tests/fixtures/schema_v1.sql`）镜像迁移 1 的 DDL，
//! 是迁移测试对旧版本 schema 的固化契约。

use nomic_core::StorageError;
use rusqlite::Connection;

/// 一条 schema 迁移：版本号与待应用的 DDL。
struct Migration {
    version: u32,
    ddl: &'static str,
}

/// 迁移链，按版本号升序；索引 + 1 即版本号。
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        ddl: "
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                parent_session TEXT,
                forked_at TEXT,
                working_dir TEXT NOT NULL
            );

            CREATE TABLE messages (
                session_id TEXT NOT NULL REFERENCES sessions (id),
                ordinal INTEGER NOT NULL,
                message_id TEXT NOT NULL,
                body TEXT NOT NULL,
                PRIMARY KEY (session_id, ordinal)
            );

            CREATE TABLE steps (
                session_id TEXT NOT NULL REFERENCES sessions (id),
                ordinal INTEGER NOT NULL,
                kind TEXT NOT NULL,
                call_id TEXT,
                message_id TEXT,
                body TEXT NOT NULL,
                PRIMARY KEY (session_id, ordinal)
            );

            CREATE TABLE submissions (
                request_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL
            );
        ",
    },
    Migration {
        version: 2,
        ddl: "ALTER TABLE sessions ADD COLUMN state TEXT NOT NULL DEFAULT 'idle';",
    },
    Migration {
        version: 3,
        ddl: "
            CREATE TABLE pending_submissions (
                request_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                body TEXT NOT NULL
            );
        ",
    },
];

/// 当前 schema 版本。
const CURRENT_VERSION: u32 = 3;

/// 把 `conn` 的 schema 迁移到当前版本。
///
/// 版本高于当前实现时返回 [`StorageError::Migration`]（库由更新
/// 版本的 release 写出，本实现无法解读）。
pub fn migrate(conn: &mut Connection) -> Result<(), StorageError> {
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    if version > CURRENT_VERSION {
        return Err(StorageError::Migration(format!(
            "schema version {version} is newer than supported version {CURRENT_VERSION}"
        )));
    }
    let tx = conn
        .transaction()
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    for migration in MIGRATIONS.iter().filter(|m| m.version > version) {
        tx.execute_batch(migration.ddl).map_err(|error| {
            StorageError::Migration(format!("migration {} failed: {error}", migration.version))
        })?;
    }
    tx.pragma_update(None, "user_version", CURRENT_VERSION)
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    tx.commit()
        .map_err(|error| StorageError::Migration(error.to_string()))?;
    Ok(())
}
