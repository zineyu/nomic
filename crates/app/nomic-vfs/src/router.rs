//! VFS 路由器：scheme → [`Vfs`] 挂载表（ADR-0042）。
//!
//! 与 oh-my-pi 的进程全局单例不同，nomic **按会话构建** router 实例
//!（工具本就按会话构造注入），VFS 的后端在构造期注入，因此无需
//! 逐次调用的上下文修正层。使用方（read/write/grep/bash 工具）共享
//! `Arc<VfsRouter>`。

use std::collections::HashMap;
use std::sync::Arc;

use crate::parse::{InternalUri, extract_uri_scheme, hierarchical_scheme, parse_internal_uri};
use crate::vfs::{Vfs, VfsEntry, VfsError, VfsFile, VfsKind, VfsMetadata};

/// VFS 路由器：内部 URI 的 scheme 挂载表。
#[derive(Default)]
pub struct VfsRouter {
    mounts: HashMap<String, Arc<dyn Vfs>>,
}

impl std::fmt::Debug for VfsRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VfsRouter")
            .field("schemes", &self.mounts.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl VfsRouter {
    /// 空挂载表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 挂载 VFS（同 scheme 后挂载覆盖先挂载）。
    pub fn mount(&mut self, vfs: Arc<dyn Vfs>) {
        self.mounts.insert(vfs.scheme().to_string(), vfs);
    }

    /// 取 scheme 对应的 VFS。
    #[must_use]
    pub fn vfs(&self, scheme: &str) -> Option<Arc<dyn Vfs>> {
        self.mounts.get(&scheme.to_ascii_lowercase()).cloned()
    }

    /// 输入是否为已挂载 scheme 的层级式内部 URI（`scheme://…`）。
    #[must_use]
    pub fn can_handle(&self, input: &str) -> bool {
        hierarchical_scheme(input).is_some_and(|scheme| self.mounts.contains_key(&scheme))
    }

    /// read 是否可解析该输入。
    ///
    /// 与 oh-my-pi 的 MCP 兜底不同，nomic 当前没有「任意自定义 scheme」的
    /// 外部资源源，因此 `can_resolve` 等价于 `can_handle`；opaque URI 形
    /// 输入一律不可解析。
    #[must_use]
    pub fn can_resolve(&self, input: &str) -> bool {
        self.can_handle(input)
    }

    /// 输入是否形似 URI（无论 scheme 是否挂载）。write 用它把
    /// 「形似但未挂载」的目标与「纯文件路径」区分开：前者报错纠错，
    /// 后者走文件系统。
    #[must_use]
    pub fn looks_like_uri(input: &str) -> bool {
        extract_uri_scheme(input).is_some()
    }

    /// 元数据分发：解析 + 派发 stat，返回盖过 immutable 章的元数据。
    /// 不读内容——元信息检查（write 闸、grep/bash 对齐）的专用入口。
    pub async fn stat(&self, input: &str) -> Result<VfsMetadata, VfsError> {
        let (url, vfs) = self.route(input)?;
        let mut meta = vfs.stat(&url).await?;
        Self::stamp(&mut meta, &*vfs);
        Ok(meta)
    }

    /// 读取分发：解析 + 派发 read，返回盖过 immutable 章的资源。
    /// 目录返回渲染清单文档（派生内容，盖章后恒不可变）。
    pub async fn read(&self, input: &str) -> Result<VfsFile, VfsError> {
        let (url, vfs) = self.route(input)?;
        let mut file = vfs.read(&url).await?;
        Self::stamp(&mut file.meta, &*vfs);
        Ok(file)
    }

    /// 目录条目分发：类型化清单（read 的清单文档之外的程序化入口）。
    pub async fn list(&self, input: &str) -> Result<Vec<VfsEntry>, VfsError> {
        let (url, vfs) = self.route(input)?;
        vfs.list(&url).await
    }

    /// 写入分发；`capabilities().writable == false` 的 scheme 报只读错误
    ///（结构性只读）。
    pub async fn write(&self, input: &str, content: &str) -> Result<(), VfsError> {
        let (url, vfs) = self.route(input)?;
        if !vfs.capabilities().writable {
            return Err(VfsError::ReadOnly { scheme: url.scheme });
        }
        vfs.write(&url, content).await
    }

    /// 支持补全的 scheme 列表（升序）。
    #[must_use]
    pub fn completion_schemes(&self) -> Vec<&str> {
        let mut schemes: Vec<&str> = self
            .mounts
            .values()
            .filter(|vfs| vfs.capabilities().completion)
            .map(|vfs| vfs.scheme())
            .collect();
        schemes.sort_unstable();
        schemes
    }

    /// immutable 盖章：单资源覆盖优先；否则只读 VFS 全量不可变，
    /// 目录清单（派生内容）恒不可变。
    fn stamp(meta: &mut VfsMetadata, vfs: &dyn Vfs) {
        meta.immutable =
            Some(meta.immutable.unwrap_or_else(|| {
                vfs.capabilities().immutable || meta.kind == VfsKind::Directory
            }));
    }

    /// 路由：解析 + 查表；未知 scheme 报附带可用列表的错误。
    fn route(&self, input: &str) -> Result<(InternalUri, Arc<dyn Vfs>), VfsError> {
        let url = parse_internal_uri(input)?;
        let Some(vfs) = self.mounts.get(&url.scheme) else {
            let supported = self
                .mounts
                .keys()
                .map(|scheme| format!("{scheme}://"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(VfsError::UnknownScheme {
                scheme: url.scheme,
                supported,
            });
        };
        Ok((url, vfs.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use crate::parse::InternalUri;
    use crate::router::VfsRouter;
    use crate::vfs::{
        ContentType, Vfs, VfsCapabilities, VfsEntry, VfsError, VfsFile, VfsKind, VfsMetadata,
    };

    struct StaticVfs {
        scheme: &'static str,
        capabilities: VfsCapabilities,
        kind: VfsKind,
        content: String,
        /// 单个资源级覆盖（Some 时优先于 VFS 默认 + 目录规则）
        resource_immutable: Option<bool>,
    }

    #[async_trait]
    impl Vfs for StaticVfs {
        fn scheme(&self) -> &'static str {
            self.scheme
        }
        fn capabilities(&self) -> VfsCapabilities {
            self.capabilities
        }
        async fn stat(&self, _uri: &InternalUri) -> Result<VfsMetadata, VfsError> {
            let mut meta = match self.kind {
                VfsKind::File => VfsMetadata::file(ContentType::Plain, None),
                VfsKind::Directory => VfsMetadata::directory(None),
            };
            meta.immutable = self.resource_immutable;
            meta.size = Some(self.content.len());
            Ok(meta)
        }
        async fn read(&self, uri: &InternalUri) -> Result<VfsFile, VfsError> {
            let meta = self.stat(uri).await?;
            Ok(VfsFile::text(
                uri.raw_href.clone(),
                self.content.clone(),
                meta,
            ))
        }
    }

    struct WritableVfs;

    #[async_trait]
    impl Vfs for WritableVfs {
        fn scheme(&self) -> &'static str {
            "local"
        }
        fn capabilities(&self) -> VfsCapabilities {
            VfsCapabilities {
                writable: true,
                immutable: false,
                completion: false,
            }
        }
        async fn stat(&self, _uri: &InternalUri) -> Result<VfsMetadata, VfsError> {
            Ok(VfsMetadata::file(ContentType::Plain, None))
        }
        async fn read(&self, uri: &InternalUri) -> Result<VfsFile, VfsError> {
            Ok(VfsFile::text(
                uri.raw_href.clone(),
                "sandbox",
                VfsMetadata::file(ContentType::Plain, None),
            ))
        }
        async fn write(&self, _uri: &InternalUri, _content: &str) -> Result<(), VfsError> {
            Ok(())
        }
    }

    const READ_ONLY: VfsCapabilities = VfsCapabilities {
        writable: false,
        immutable: true,
        completion: false,
    };
    const READ_WRITE: VfsCapabilities = VfsCapabilities {
        writable: true,
        immutable: false,
        completion: false,
    };

    fn router() -> VfsRouter {
        let mut router = VfsRouter::new();
        router.mount(Arc::new(StaticVfs {
            scheme: "skill",
            capabilities: READ_ONLY,
            kind: VfsKind::File,
            content: "skill body".into(),
            resource_immutable: None,
        }));
        router.mount(Arc::new(WritableVfs));
        router
    }

    #[test]
    fn can_handle_only_mounted_hierarchical() {
        let router = router();
        assert!(router.can_handle("skill://pdf"));
        assert!(router.can_handle("SKILL://pdf")); // scheme 大小写不敏感
        assert!(!router.can_handle("missing://x"));
        assert!(!router.can_handle("skill:opaque")); // opaque 形式不可路由
        assert!(!router.can_handle("src/main.rs"));
        assert!(router.can_resolve("local://a.md"));
        assert!(VfsRouter::looks_like_uri("missing://x"));
        assert!(!VfsRouter::looks_like_uri("src/main.rs"));
    }

    #[tokio::test]
    async fn read_stamps_vfs_immutable_default() {
        let router = router();
        let file = router.read("skill://pdf").await.expect("read");
        assert!(file.meta.is_immutable()); // VFS 默认 true
        assert_eq!(file.content, "skill body");

        let file = router.read("local://a.md").await.expect("read");
        assert!(!file.meta.is_immutable()); // VFS 默认 false
    }

    #[tokio::test]
    async fn stat_returns_metadata_without_read_dispatch() {
        let router = router();
        let meta = router.stat("skill://pdf").await.expect("stat");
        assert_eq!(meta.kind, VfsKind::File);
        assert_eq!(meta.size, Some(10));
        assert!(meta.is_immutable());
    }

    #[tokio::test]
    async fn directory_is_stamped_immutable_even_on_writable_vfs() {
        let mut router = VfsRouter::new();
        router.mount(Arc::new(StaticVfs {
            scheme: "vault",
            capabilities: READ_WRITE,
            kind: VfsKind::Directory,
            content: "listing".into(),
            resource_immutable: None,
        }));
        let meta = router.stat("vault://work/").await.expect("stat");
        assert!(meta.is_immutable()); // 目录清单恒不可变
    }

    #[tokio::test]
    async fn resource_level_immutable_overrides_default() {
        let mut router = VfsRouter::new();
        router.mount(Arc::new(StaticVfs {
            scheme: "vault",
            capabilities: READ_WRITE,
            kind: VfsKind::File,
            content: "listing".into(),
            resource_immutable: Some(true), // 派生内容覆盖为不可变
        }));
        let file = router.read("vault://work").await.expect("read");
        assert!(file.meta.is_immutable());
    }

    #[tokio::test]
    async fn write_routes_to_writable_vfs_only() {
        let router = router();
        router
            .write("local://a.md", "content")
            .await
            .expect("write");

        let error = router.write("skill://pdf", "content").await.unwrap_err();
        assert!(error.to_string().contains("read-only"), "{error}");
    }

    #[tokio::test]
    async fn unknown_scheme_error_lists_supported() {
        let router = router();
        let error = router.read("bogus://x").await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Unknown protocol: bogus://"), "{message}");
        assert!(message.contains("skill://"), "{message}");
        assert!(message.contains("local://"), "{message}");
    }

    #[tokio::test]
    async fn invalid_uri_is_rejected() {
        let router = router();
        assert!(router.read("not a uri").await.is_err());
    }

    #[tokio::test]
    async fn list_dispatches_and_default_errors() {
        struct ListableVfs;
        #[async_trait]
        impl Vfs for ListableVfs {
            fn scheme(&self) -> &'static str {
                "ls"
            }
            fn capabilities(&self) -> VfsCapabilities {
                READ_ONLY
            }
            async fn stat(&self, _uri: &InternalUri) -> Result<VfsMetadata, VfsError> {
                Ok(VfsMetadata::directory(None))
            }
            async fn read(&self, uri: &InternalUri) -> Result<VfsFile, VfsError> {
                let entries = self.list(uri).await?;
                Ok(VfsFile::text(
                    uri.raw_href.clone(),
                    crate::vfs::render_listing(&entries),
                    VfsMetadata::directory(None),
                ))
            }
            async fn list(&self, _uri: &InternalUri) -> Result<Vec<VfsEntry>, VfsError> {
                Ok(vec![
                    VfsEntry {
                        name: "sub".into(),
                        kind: VfsKind::Directory,
                    },
                    VfsEntry {
                        name: "a.md".into(),
                        kind: VfsKind::File,
                    },
                ])
            }
        }

        let mut router = VfsRouter::new();
        router.mount(Arc::new(ListableVfs));
        router.mount(Arc::new(WritableVfs));
        let entries = router.list("ls://root").await.expect("list");
        assert_eq!(entries.len(), 2);
        // 目录 read 的渲染契约：目录在前带 `/`
        let file = router.read("ls://root").await.expect("read");
        assert_eq!(file.content, "sub/\na.md");
        assert!(file.meta.is_immutable());

        // 未覆盖 list 的 VFS 报「不支持目录清单」
        let error = router.list("local://a.md").await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not support directory listing"),
            "{error}"
        );
    }
}
