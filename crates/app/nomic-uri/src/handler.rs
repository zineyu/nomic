//! 协议处理器契约与资源类型。

use std::path::PathBuf;

use async_trait::async_trait;
use thiserror::Error;

use crate::parse::InternalUri;

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
    #[error(
        "{scheme}:// URLs are read-only for write; use the protocol-specific tool for mutations."
    )]
    ReadOnly {
        /// 只读 scheme
        scheme: String,
    },
    /// 协议 handler 解析/写入失败（用户友好消息）
    #[error("{0}")]
    Resolve(String),
}

/// 资源内容的 MIME 类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// text/markdown
    Markdown,
    /// application/json
    Json,
    /// text/plain
    Plain,
}

/// 协议 handler 返回的资源载荷。
///
/// `immutable` 为 `None` 时由 router 盖 handler 的默认章
///（[`ProtocolHandler::immutable`]）；handler 可对单个资源覆盖（例如目录
/// 清单、索引页等派生内容，即使文件型协议可变，清单也必须不可编辑）。
#[derive(Debug, Clone)]
pub struct UriResource {
    /// 规范化后的 URL
    pub url: String,
    /// 文本内容
    pub content: String,
    /// 内容类别
    pub content_type: ContentType,
    /// 底层文件系统路径（grep/bash 用；虚拟资源为 `None`）
    pub source_path: Option<PathBuf>,
    /// 单个资源的不可变覆盖；`None` = 用 handler 默认值
    pub immutable: Option<bool>,
    /// handler 附带的结构化元数据（如 skill 来源标注），由 read 工具
    /// 合并进 `ToolResult.details`。默认无。
    pub details: Option<serde_json::Value>,
    /// 是否为目录清单而非文件内容
    pub is_directory: bool,
    /// 解析附加说明（缓存新鲜度、来源等）
    pub notes: Vec<String>,
}

impl UriResource {
    /// 便捷构造：纯文本资源。
    #[must_use]
    pub fn text(url: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            content: content.into(),
            content_type: ContentType::Plain,
            source_path: None,
            immutable: None,
            details: None,
            is_directory: false,
            notes: Vec::new(),
        }
    }

    /// 生效的不可变标记（router 盖章后总有值；未盖章时 `false`）。
    #[must_use]
    pub fn is_immutable(&self) -> bool {
        self.immutable.unwrap_or(false)
    }

    /// 内容字节数。
    #[must_use]
    pub const fn size(&self) -> usize {
        self.content.len()
    }
}

/// 一个 `scheme://` 补全候选（host/path 段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlCompletion {
    /// `scheme://` 之后的文本（如 `pdf`、`subdir/data.json`）
    pub value: String,
    /// 人类可读标签（默认展示 value）
    pub label: Option<String>,
    /// 候选旁的一行描述
    pub description: Option<String>,
}

/// 一个内部 URI scheme 的处理器。
///
/// 实现必须是**无状态的**或只持有构造期注入的会话后端（skill resolver、
/// artifacts 目录等）；router 按会话构建，天然会话隔离。
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// 本 handler 处理的 scheme（不含 `://`，小写）
    fn scheme(&self) -> &'static str;

    /// 本协议产出资源的默认不可变标记。`true` 时 read 结果抑制编辑
    /// 提示、write/edit 拒绝修改。
    fn immutable(&self) -> bool;

    /// 把内部 URI 解析为内容资源。
    ///
    /// 错误必须是用户（模型）友好消息：指出哪里错了、可用什么。
    async fn resolve(&self, url: &InternalUri) -> Result<UriResource, UriError>;

    /// 是否实现写入。`true` 的 handler 必须同时覆盖 [`Self::write`]。
    fn writable(&self) -> bool {
        false
    }

    /// 写入内部 URI。缺省实现报只读错误——**只读性是结构性的**：
    /// 不覆盖本方法的协议天然不可写。
    async fn write(&self, _url: &InternalUri, _content: &str) -> Result<(), UriError> {
        Err(UriError::ReadOnly {
            scheme: self.scheme().to_string(),
        })
    }

    /// 可选补全：host/path 段候选。**必须快且本地**——可能在每次击键时
    /// 运行；网络/外部 CLI 后端的协议不应实现。返回全量（有界）候选，
    /// 调用方负责按 query 模糊过滤。实现本方法的 handler 必须同时让
    /// [`Self::supports_completion`] 返回 `true`。
    fn complete(&self, _query: &str) -> Vec<UrlCompletion> {
        Vec::new()
    }

    /// 是否实现了 [`Self::complete`]（trait 默认实现无法探测覆盖，
    /// 由 handler 显式自报）。
    fn supports_completion(&self) -> bool {
        false
    }
}
