//! 虚拟文件系统契约（ADR-0042）：一个 URI scheme 对应一个 [`Vfs`] 实现。
//!
//! 操作集对齐文件系统语义：[`Vfs::stat`]（元数据，不读内容）、
//! [`Vfs::read`]（内容；目录返回渲染清单文档）、[`Vfs::list`]（类型化
//! 目录条目）、[`Vfs::write`]（缺省只读——只读性是结构性的）。
//! 能力（可写/默认不可变/可补全）由 [`VfsCapabilities`] 单点声明，
//! 取代 ADR-0040 的 `immutable()` + `writable()` + `supports_completion()`
//! 三方法组合。

use std::path::PathBuf;

use async_trait::async_trait;
use thiserror::Error;

use crate::parse::InternalUri;

/// VFS 系统错误。错误文本面向模型：回喂后应能自我修正。
#[derive(Debug, Error)]
pub enum VfsError {
    /// 输入不是合法的层级式内部 URI
    #[error("Invalid URI: {0}")]
    InvalidUri(String),
    /// scheme 未挂载
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
    /// VFS 实现解析/写入失败（用户友好消息）
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

/// 资源种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsKind {
    /// 普通文件（read 返回内容）
    File,
    /// 目录（read 返回渲染清单文档；list 返回类型化条目）
    Directory,
}

/// 挂载能力声明（结构性，随 scheme 固定）。
// 三枚 bool 是正交能力位，值语义清晰，不引 bitflags
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VfsCapabilities {
    /// write 是否可用；`false` = 结构性只读，router 直接拒绝写入
    pub writable: bool,
    /// 该 VFS 产出的资源默认不可变（router 盖章；目录清单恒不可变，
    /// 无需在此声明）
    pub immutable: bool,
    /// 是否实现了 [`Vfs::complete`]
    pub completion: bool,
}

impl VfsCapabilities {
    /// 只读 + 可补全（skill 等提示词资产）。
    pub const READ_ONLY_COMPLETION: Self = Self {
        writable: false,
        immutable: true,
        completion: true,
    };

    /// 可写 + 可补全（local/nix 等工作区资源）。
    pub const READ_WRITE_COMPLETION: Self = Self {
        writable: true,
        immutable: false,
        completion: true,
    };
}

/// [`Vfs::stat`] 的结果：资源元数据（不读内容）。
#[derive(Debug, Clone)]
pub struct VfsMetadata {
    /// 文件 / 目录
    pub kind: VfsKind,
    /// 内容类别（目录清单恒为 [`ContentType::Plain`]）
    pub content_type: ContentType,
    /// 底层文件系统路径（grep/bash 用）。目录化不变量下恒有值：
    /// 每个挂载的 scheme 都有 backing root（ADR-0043）。
    pub source_path: PathBuf,
    /// 单个资源的不可变覆盖；`None` = 用 VFS 默认 + 目录规则（router 盖章）
    pub immutable: Option<bool>,
    /// 内容字节数（已知时）
    pub size: Option<usize>,
}

impl VfsMetadata {
    /// 便捷构造：文件元数据。
    #[must_use]
    pub const fn file(content_type: ContentType, source_path: PathBuf) -> Self {
        Self {
            kind: VfsKind::File,
            content_type,
            source_path,
            immutable: None,
            size: None,
        }
    }

    /// 便捷构造：目录元数据。
    #[must_use]
    pub const fn directory(source_path: PathBuf) -> Self {
        Self {
            kind: VfsKind::Directory,
            content_type: ContentType::Plain,
            source_path,
            immutable: None,
            size: None,
        }
    }

    /// 生效的不可变标记（router 盖章后总有值；未盖章时 `false`）。
    #[must_use]
    pub fn is_immutable(&self) -> bool {
        self.immutable.unwrap_or(false)
    }
}

/// [`Vfs::list`] 的类型化目录条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsEntry {
    /// 条目名（不含路径分隔符、不含尾部斜杠）
    pub name: String,
    /// 条目种类
    pub kind: VfsKind,
}

/// [`Vfs::read`] 的结果：内容 + 元数据 + 注解。
#[derive(Debug, Clone)]
pub struct VfsFile {
    /// 规范化后的 URL
    pub url: String,
    /// 文本内容（目录时为 [`render_listing`] 渲染的清单文档）
    pub content: String,
    /// 资源元数据
    pub meta: VfsMetadata,
    /// VFS 附带的结构化元数据（如 skill 来源标注），由 read 工具
    /// 合并进 `ToolResult.details`。默认无。
    pub details: Option<serde_json::Value>,
    /// 解析附加说明（缓存新鲜度、来源等）
    pub notes: Vec<String>,
}

impl VfsFile {
    /// 便捷构造：纯文本资源。
    #[must_use]
    pub fn text(url: impl Into<String>, content: impl Into<String>, meta: VfsMetadata) -> Self {
        Self {
            url: url.into(),
            content: content.into(),
            meta,
            details: None,
            notes: Vec::new(),
        }
    }

    /// 内容字节数。
    #[must_use]
    pub const fn size(&self) -> usize {
        self.content.len()
    }
}

/// 渲染目录清单文档：目录在前（带尾部 `/`）、按名排序、一行一个条目。
/// 所有 VFS 的目录 `read` 共用的输出契约（与 ADR-0040 时代的清单格式
/// 逐字节一致）。
#[must_use]
pub fn render_listing(entries: &[VfsEntry]) -> String {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries {
        match entry.kind {
            VfsKind::Directory => dirs.push(format!("{}/", entry.name)),
            VfsKind::File => files.push(entry.name.clone()),
        }
    }
    dirs.sort();
    files.sort();
    dirs.into_iter().chain(files).collect::<Vec<_>>().join("\n")
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

/// 一个内部 URI scheme 的虚拟文件系统实现。
///
/// 实现必须是**无状态的**或只持有构造期注入的会话后端（skill resolver、
/// workspace 根等）；router 按会话构建，天然会话隔离。
#[async_trait]
pub trait Vfs: Send + Sync {
    /// 本 VFS 挂载的 scheme（不含 `://`，小写）
    fn scheme(&self) -> &'static str;

    /// 挂载能力声明。
    fn capabilities(&self) -> VfsCapabilities;

    /// 取资源元数据（不读内容）。资源不存在时报 [`VfsError::Resolve`]。
    ///
    /// 错误必须是用户（模型）友好消息：指出哪里错了、可用什么。
    async fn stat(&self, uri: &InternalUri) -> Result<VfsMetadata, VfsError>;

    /// 读取资源内容。目录返回 [`render_listing`] 渲染的清单文档
    /// （派生内容，router 盖不可变章）；错误消息要求同 [`Vfs::stat`]。
    async fn read(&self, uri: &InternalUri) -> Result<VfsFile, VfsError>;

    /// 列出目录的类型化条目。缺省实现报「不支持目录清单」——纯文件型
    /// VFS（如 `nix://`）无需覆盖。实现方负责有界（条目数上限）。
    async fn list(&self, uri: &InternalUri) -> Result<Vec<VfsEntry>, VfsError> {
        Err(VfsError::Resolve(format!(
            "{}:// does not support directory listing: {}",
            self.scheme(),
            uri.without_query()
        )))
    }

    /// 写入资源。缺省实现报只读错误——**只读性是结构性的**：
    /// 不覆盖本方法且 `capabilities().writable == false` 的 VFS 天然不可写。
    async fn write(&self, _uri: &InternalUri, _content: &str) -> Result<(), VfsError> {
        Err(VfsError::ReadOnly {
            scheme: self.scheme().to_string(),
        })
    }

    /// 可选补全：host/path 段候选。**必须快且本地**——可能在每次击键时
    /// 运行；网络/外部 CLI 后端的 VFS 不应实现。返回全量（有界）候选，
    /// 调用方负责按 query 模糊过滤。实现本方法的 VFS 必须让
    /// [`VfsCapabilities::completion`] 为 `true`。
    fn complete(&self, _query: &str) -> Vec<UrlCompletion> {
        Vec::new()
    }

    /// 系统提示词用的一行语义描述（`None` = 不进提示词）。
    /// 目录化挂载（[`crate::DirMount`]）恒有描述——模型必须知道已挂载
    /// 的 prefix；即席/测试 VFS 可保持隐藏。
    fn describe(&self) -> Option<&'static str> {
        None
    }
}
