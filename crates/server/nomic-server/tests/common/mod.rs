//! 存储 seam 测试共享工具。
//!
//! 测试经 `nomic_core::Storage` trait 驱动临时目录上的真实 SQLite。

use std::path::Path;

use nomic_server::storage::SQLiteStorage;

/// 在临时目录下创建真实 SQLite 存储。
pub fn open_storage(dir: &Path) -> SQLiteStorage {
    SQLiteStorage::open(dir.join("nomic.db")).unwrap()
}
