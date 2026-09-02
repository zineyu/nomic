//! `nomic-vfs`：内部 URI 寻址的虚拟文件系统（ADR-0042，前身为 ADR-0040
//! 的 `nomic-uri`）。
//!
//! 把「agent 可寻址的非普通文件资源」统一抽象为 `scheme://` 内部 URI，
//! 由 [`VfsRouter`] 按 scheme 挂载分发——**一个 URI 种类对应一个
//! [`Vfs`] 具体实现**（`fs::SkillVfs` / `fs::LocalVfs` / `fs::NixVfs`）。
//!
//! 分层：
//! - [`parse`] / `selector`：寻址层（容错 URI 解析、尾挂选择器）；
//! - [`vfs`]：契约层（[`Vfs`] trait、[`VfsCapabilities`]、[`VfsMetadata`]、
//!   [`VfsFile`]、[`VfsEntry`]）；
//! - [`mount`]：目录化挂载层（[`Mount`] 声明 + [`DirMount`] 适配器，
//!   ADR-0043——每个 scheme 都是一个 backing root 目录）；
//! - [`router`]：挂载表层（immutable 盖章、未知 scheme 纠错）；
//! - [`fs`]：内置挂载声明（[`fs::LocalMount`] / [`fs::NixMount`] /
//!   [`fs::SkillMount`]，经 [`DirMount`] 获得完整 [`Vfs`] 语义）。

pub mod fs;
pub mod mount;
pub mod parse;
mod root;
pub mod router;
mod selector;
pub mod vfs;

pub use mount::{DirMount, Mount};
pub use parse::{InternalUri, extract_uri_scheme, parse_internal_uri};
pub use root::WorkspaceRoot;
pub use router::VfsRouter;
pub use selector::{LineRange, ParsedSelector, SelectorError, parse_selector, split_uri_selector};
pub use vfs::{
    ContentType, UrlCompletion, Vfs, VfsCapabilities, VfsEntry, VfsError, VfsFile, VfsKind,
    VfsMetadata, render_listing,
};
