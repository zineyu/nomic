//! 目录化挂载（ADR-0043）：[`Mount`] 声明 + [`DirMount`] 适配器。
//!
//! 核心不变量：**每个挂载的 scheme 都有一个 backing root 目录，scheme
//! 下的每个 URI 映射为其中一个文件系统路径**。[`Mount`] 是纯声明
//! （scheme、能力位、钩子），不含 I/O；[`DirMount`] 把声明适配为完整
//! [`Vfs`]，统一实现 stat/read/list/write，收编原先散在各实现里的
//! 公共行为：目录清单渲染、有界条目、content-type 推断、写入前创建
//! 父目录、统一错误文案。
//!
//! 新增协议的边际成本 = 一个 [`Mount`] 声明（`locate` + 能力位 +
//! 按需钩子），无 I/O 代码。

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::fs::{MAX_LISTING_ENTRIES, content_type_for, dir_entries};
use crate::parse::InternalUri;
use crate::vfs::{
    UrlCompletion, Vfs, VfsCapabilities, VfsEntry, VfsError, VfsFile, VfsMetadata, render_listing,
};

/// 目录化挂载声明：一个 scheme 的最小语义面。
///
/// 实现必须是**无状态的**或只持有构造期注入的后端（project 根句柄、
/// skill resolver 等）；钩子均为同步——文件系统 I/O 由 [`DirMount`]
/// 统一执行。
pub trait Mount: Send + Sync {
    /// 挂载的 scheme（不含 `://`，小写）
    fn scheme(&self) -> &'static str;

    /// 挂载能力声明（语义同 [`Vfs::capabilities`]）。
    fn capabilities(&self) -> VfsCapabilities;

    /// URI → backing 目录内的绝对路径。
    ///
    /// 实现方负责 containment（拒绝越出根的穿越）与「非法资源」的
    /// 引导文案；目标不存在不在这里报（由 [`Mount::not_found`] 表达）。
    fn locate(&self, uri: &InternalUri) -> Result<PathBuf, VfsError>;

    /// 目录的索引文件：read/stat 以索引文件代表该目录（list 仍列条目）。
    /// 默认无索引。`skill://` 用它把根映射到 SKILL.md。
    fn index(&self, _uri: &InternalUri) -> Option<&'static str> {
        None
    }

    /// read 之后的内容/details 变换（默认原样）。
    ///
    /// 变换只发生在 read；stat 恒报告背书文件的事实
    ///（ADR-0043 §语义对齐）。
    fn transform(&self, _uri: &InternalUri, file: VfsFile) -> VfsFile {
        file
    }

    /// 定位后目标不存在的错误；默认 `Could not resolve <href>. <io>`。
    /// 可写挂载可覆盖为「写入即创建」的引导文案（如 `nix://`）。
    fn not_found(&self, uri: &InternalUri, _path: &Path, error: &std::io::Error) -> VfsError {
        VfsError::Resolve(format!(
            "Could not resolve {}. {error}",
            uri.without_query()
        ))
    }

    /// 是否支持目录清单（默认支持；纯文件型挂载如 `nix://` 关闭）。
    fn supports_listing(&self) -> bool {
        true
    }

    /// 系统提示词用的一行语义描述（必填——模型必须知道已挂载的
    /// prefix；读写性由 router 渲染时按能力位附加）。
    fn describe(&self) -> &'static str;

    /// 可选补全（语义同 [`Vfs::complete`]）：**必须快且本地**。实现
    /// 本方法的挂载必须让 [`VfsCapabilities::completion`] 为 `true`。
    fn complete(&self, _query: &str) -> Vec<UrlCompletion> {
        Vec::new()
    }
}

/// 把 [`Mount`] 声明适配为完整 [`Vfs`]（ADR-0043）。
///
/// 统一语义：
/// - read：目录 → 索引文件（声明且存在时）或 [`render_listing`] 清单
///   文档；文件 → 内容 + [`Mount::transform`]；读出的文件元数据在
///   transform 后对齐 `size`；
/// - stat：纯元数据，不读内容、不做 transform——索引目录报告索引
///   文件的事实；
/// - write：写入前创建父目录；目录目标由文件系统报错；
/// - 错误文案统一：`Could not resolve <href>. <io>`（可由
///   [`Mount::not_found`] 覆盖）、`<href> is a file, not a directory.`、
///   `Could not create parent directories for <href>: <io>`、
///   `Could not write <href>. <io>`；
/// - `VfsFile.url` 统一为不含 query 的 href（query 是选择器参数，
///   非资源标识）。
pub struct DirMount<M> {
    decl: M,
}

impl<M: Mount> std::fmt::Debug for DirMount<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirMount")
            .field("scheme", &self.decl.scheme())
            .finish_non_exhaustive()
    }
}

impl<M: Mount> DirMount<M> {
    /// 以挂载声明构造。
    pub const fn from_decl(decl: M) -> Self {
        Self { decl }
    }

    /// locate + metadata；不存在时经 [`Mount::not_found`] 报错。
    async fn locate_metadata(
        &self,
        uri: &InternalUri,
    ) -> Result<(PathBuf, std::fs::Metadata), VfsError> {
        let path = self.decl.locate(uri)?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| self.decl.not_found(uri, &path, &error))?;
        Ok((path, metadata))
    }

    /// 目录 + 声明索引且索引文件存在 → 索引文件路径与元数据。
    async fn index_file(
        &self,
        uri: &InternalUri,
        dir: &Path,
    ) -> Option<(PathBuf, std::fs::Metadata)> {
        let index = self.decl.index(uri)?;
        let path = dir.join(index);
        let metadata = tokio::fs::metadata(&path).await.ok()?;
        metadata.is_file().then_some((path, metadata))
    }

    /// 读取文件内容并组装 [`VfsFile`]（transform 后对齐 size）。
    async fn read_file(
        &self,
        uri: &InternalUri,
        href: &str,
        path: PathBuf,
    ) -> Result<VfsFile, VfsError> {
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))?;
        let meta = VfsMetadata::file(content_type_for(&path), path);
        let mut file = self.decl.transform(uri, VfsFile::text(href, content, meta));
        file.meta.size = Some(file.size());
        Ok(file)
    }
}

#[async_trait]
impl<M: Mount> Vfs for DirMount<M> {
    fn scheme(&self) -> &'static str {
        self.decl.scheme()
    }

    fn capabilities(&self) -> VfsCapabilities {
        self.decl.capabilities()
    }

    async fn stat(&self, uri: &InternalUri) -> Result<VfsMetadata, VfsError> {
        let (path, metadata) = self.locate_metadata(uri).await?;
        if metadata.is_dir() {
            if let Some((index_path, index_meta)) = self.index_file(uri, &path).await {
                let mut meta = VfsMetadata::file(content_type_for(&index_path), index_path);
                meta.size = usize::try_from(index_meta.len()).ok();
                return Ok(meta);
            }
            return Ok(VfsMetadata::directory(path));
        }
        let mut meta = VfsMetadata::file(content_type_for(&path), path);
        meta.size = usize::try_from(metadata.len()).ok();
        Ok(meta)
    }

    async fn read(&self, uri: &InternalUri) -> Result<VfsFile, VfsError> {
        let href = uri.without_query();
        let (path, metadata) = self.locate_metadata(uri).await?;
        if metadata.is_dir() {
            if let Some((index_path, _)) = self.index_file(uri, &path).await {
                return self.read_file(uri, &href, index_path).await;
            }
            let entries = dir_entries(&path, MAX_LISTING_ENTRIES)
                .await
                .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))?;
            let file = VfsFile::text(href, render_listing(&entries), VfsMetadata::directory(path));
            return Ok(self.decl.transform(uri, file));
        }
        self.read_file(uri, &href, path).await
    }

    async fn list(&self, uri: &InternalUri) -> Result<Vec<VfsEntry>, VfsError> {
        if !self.decl.supports_listing() {
            return Err(VfsError::Resolve(format!(
                "{}:// does not support directory listing: {}",
                self.decl.scheme(),
                uri.without_query()
            )));
        }
        let href = uri.without_query();
        let (path, metadata) = self.locate_metadata(uri).await?;
        if !metadata.is_dir() {
            return Err(VfsError::Resolve(format!(
                "{href} is a file, not a directory."
            )));
        }
        dir_entries(&path, MAX_LISTING_ENTRIES)
            .await
            .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))
    }

    async fn write(&self, uri: &InternalUri, content: &str) -> Result<(), VfsError> {
        let href = uri.without_query();
        let path = self.decl.locate(uri)?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                VfsError::Resolve(format!(
                    "Could not create parent directories for {href}: {error}"
                ))
            })?;
        }
        tokio::fs::write(&path, content)
            .await
            .map_err(|error| VfsError::Resolve(format!("Could not write {href}. {error}")))
    }

    fn complete(&self, query: &str) -> Vec<UrlCompletion> {
        self.decl.complete(query)
    }

    fn describe(&self) -> Option<&'static str> {
        Some(self.decl.describe())
    }
}
