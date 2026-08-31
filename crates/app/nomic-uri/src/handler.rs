//! 协议处理器契约与资源类型（T2 扩展为完整 trait）。

use thiserror::Error;

/// URI 系统错误。错误文本面向模型：回喂后应能自我修正。
#[derive(Debug, Error)]
pub enum UriError {
    /// 输入不是合法的层级式内部 URI
    #[error("Invalid URI: {0}")]
    InvalidUri(String),
    /// scheme 未注册
    #[error("Unknown protocol: {scheme}://\nSupported: {supported}")]
    UnknownScheme {
        /// 未识别的 scheme
        scheme: String,
        /// 逗号分隔的可用 scheme 列表
        supported: String,
    },
    /// 协议存在但为只读
    #[error("{scheme}:// URLs are read-only for write; use the protocol-specific tool for mutations.")]
    ReadOnly {
        /// 只读 scheme
        scheme: String,
    },
    /// 协议 handler 解析失败（用户友好消息）
    #[error("{0}")]
    Resolve(String),
}
