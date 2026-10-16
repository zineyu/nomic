//! SQLite 存储实现：[`nomic_core::Storage`] 的首个具体实现。
//!
//! server 全库只经 `Storage` trait 访问本模块；schema 版本迁移在
//! `schema` 子模块中集中管理。

mod schema;
mod sqlite;

pub use sqlite::SQLiteStorage;
