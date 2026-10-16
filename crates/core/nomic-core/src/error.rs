//! 错误类型。
//!
//! 每个边界一个错误类型，变体即失败类别；调用方按类别决定
//! 处理策略（如重试只覆盖 [`LlmError::Transport`]）。

/// 存储层故障。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageError {
    /// 打不开存储或连接丢失。
    #[error("storage connection failed: {0}")]
    Connection(String),
    /// 查询或写入执行失败。
    #[error("storage query failed: {0}")]
    Query(String),
    /// schema 版本迁移失败。
    #[error("schema migration failed: {0}")]
    Migration(String),
    /// 操作的目标不存在（如向已删除的会话追加步骤）。
    #[error("target not found: {0}")]
    NotFound(String),
}

/// LLM 适配层故障。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LlmError {
    /// 网络层瞬时错误：重试策略只覆盖本类别（≤2 次指数退避）。
    #[error("transport error: {0}")]
    Transport(String),
    /// provider 拒绝或返回无法解析的响应。
    #[error("provider error: {0}")]
    Provider(String),
    /// 达到上下文窗口上限：agent 停止，用户可分叉续跑。
    #[error("context window limit reached")]
    ContextLimit,
    /// 进行中的请求被协作式取消。
    #[error("request cancelled")]
    Cancelled,
}

/// 执行环境自身故障。
///
/// 工具语义层面的成功/拒绝经 [`crate::ToolResult`] 表达；
/// 本类型只表示环境无法完成派发（如进程无法拉起）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EnvironmentError {
    /// 工作进程拉起失败。
    #[error("failed to spawn: {0}")]
    Spawn(String),
    /// 环境 IO 故障。
    #[error("environment io error: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 错误类别的_display_含类别前缀() {
        assert!(
            StorageError::Connection("db".into())
                .to_string()
                .contains("connection")
        );
        assert!(
            LlmError::Transport("timeout".into())
                .to_string()
                .contains("transport")
        );
        assert!(
            EnvironmentError::Io("pipe".into())
                .to_string()
                .contains("io")
        );
    }

    #[test]
    fn 瞬时错误与取消可区分() {
        let transport = LlmError::Transport("timeout".into());
        let cancelled = LlmError::Cancelled;
        assert!(matches!(transport, LlmError::Transport(_)));
        assert!(matches!(cancelled, LlmError::Cancelled));
    }
}
