//! session 存储层的错误类型。

/// session 存储层的错误。
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// SQLite 运行时错误
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// 迁移执行失败
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// 文件系统错误（创建目录、解析默认路径等）
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// session id 不存在
    #[error("session not found: {0}")]
    SessionNotFound(String),
    /// work id 不存在
    #[error("work not found: {0}")]
    WorkNotFound(String),
    /// project 不存在（`get_or_create_project` 登记后读取仍缺失，
    /// 仅在库被并发破坏时可能出现）
    #[error("project not found: {0}")]
    ProjectNotFound(String),
    /// project 下仍有 session，非 force 删除被拒（只统计有 user 消息的
    /// session；空壳不拦截删除，随 project 一并清除）
    #[error("project {id} 下仍有 {count} 个 session（force 可级联删除）")]
    ProjectNotEmpty {
        /// project id
        id: String,
        /// 有 user 消息的 session 数
        count: u64,
    },
    /// entry id 不存在（或不属于目标 session）
    #[error("entry not found: {0}")]
    EntryNotFound(String),
    /// 库中 payload 不是合法的 [`Entry`](nomic_ai::Entry) JSON（数据损坏）
    #[error("entry payload corrupted: {0}")]
    Corrupt(#[from] serde_json::Error),
    /// 库中 payload 的 role/parts 组合非法（数据损坏）
    #[error("entry payload invalid: {0}")]
    InvalidEntry(#[from] nomic_ai::EntryError),
}
