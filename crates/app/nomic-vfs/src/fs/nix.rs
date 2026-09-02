//! `nix://` 挂载：workspace 级 nix 环境定义（ADR-0041，ADR-0042 VFS 化，
//! ADR-0043 目录化挂载）。
//!
//! - `nix://shell` → `<workspace>/.nomic/flake.nix`（唯一资源），可读写：
//!   agent 经 write/edit 修改环境定义；bash 工具的 env 缓存按 mtime 失效，
//!   无需跨组件通知。
//! - 纯文件型挂载：`supports_listing = false`（DirMount 报「不支持目录
//!   清单」）。
//! - 其余路径报 Resolve 错误并列出可用资源。
//! - 背书路径即底层真实路径，grep/bash 据此与 fs 路径对齐。
//!
//! 全部文件系统语义由 [`DirMount`] 统一实现；本模块只声明 `locate`
//! （`shell` → `flake.nix` 映射）、「写入即创建」引导与补全钩子。

use std::path::{Path, PathBuf};

use crate::mount::{DirMount, Mount};
use crate::parse::InternalUri;
use crate::root::WorkspaceRoot;
use crate::vfs::{UrlCompletion, VfsCapabilities, VfsError};

/// `nix://` 的挂载声明。
#[derive(Debug)]
pub struct NixMount {
    root: WorkspaceRoot,
}

impl NixMount {
    /// `nix://shell` → `<workspace>/.nomic/flake.nix`；其余路径报错。
    fn resolve_path(&self, uri: &InternalUri) -> Result<PathBuf, VfsError> {
        if uri.raw_host != "shell" || !uri.raw_path.is_empty() {
            return Err(VfsError::Resolve(format!(
                "Unknown nix:// resource: {}. Available: nix://shell",
                uri.without_query()
            )));
        }
        let root = self
            .root
            .snapshot()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(root.join(".nomic").join("flake.nix"))
    }
}

impl Mount for NixMount {
    fn scheme(&self) -> &'static str {
        "nix"
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities::READ_WRITE_COMPLETION
    }

    fn locate(&self, uri: &InternalUri) -> Result<PathBuf, VfsError> {
        self.resolve_path(uri)
    }

    fn describe(&self) -> &'static str {
        "workspace nix environment definition; nix://shell maps to .nomic/flake.nix \
         (write it to install tools into the bash environment)"
    }

    fn not_found(&self, uri: &InternalUri, path: &Path, _error: &std::io::Error) -> VfsError {
        VfsError::Resolve(format!(
            "Could not resolve {}: {} does not exist yet. \
             Write nix://shell to create the workspace nix environment definition.",
            uri.without_query(),
            path.display()
        ))
    }

    fn supports_listing(&self) -> bool {
        false
    }

    fn complete(&self, query: &str) -> Vec<UrlCompletion> {
        if "shell".starts_with(query) {
            vec![UrlCompletion {
                value: "shell".to_string(),
                label: None,
                description: Some("workspace nix environment (.nomic/flake.nix)".to_string()),
            }]
        } else {
            Vec::new()
        }
    }
}

/// `nix://shell` VFS：workspace `.nomic/flake.nix` 的可读写目录化挂载
///（ADR-0043）。
pub type NixVfs = DirMount<NixMount>;

impl DirMount<NixMount> {
    /// 以共享 workspace 根句柄构造。
    #[must_use]
    pub const fn new(root: WorkspaceRoot) -> Self {
        Self::from_decl(NixMount { root })
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::parse::parse_internal_uri;
    use crate::vfs::{Vfs, VfsKind};

    struct Fixture {
        _dir: TempDir,
        vfs: NixVfs,
        flake_path: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().expect("temp dir");
        let flake_path = dir.path().join(".nomic").join("flake.nix");
        let root = WorkspaceRoot::new(Some(dir.path().to_path_buf()));
        Fixture {
            _dir: dir,
            vfs: NixVfs::new(root),
            flake_path,
        }
    }

    fn uri(input: &str) -> InternalUri {
        parse_internal_uri(input).expect("parse")
    }

    #[tokio::test]
    async fn stat_missing_flake_hints_write_to_create() {
        let fixture = fixture();
        let error = fixture.vfs.stat(&uri("nix://shell")).await.unwrap_err();
        assert!(error.to_string().contains("nix://shell"), "{error}");
        assert!(
            error.to_string().contains("Write nix://shell to create"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn write_creates_flake_and_roundtrips() {
        let fixture = fixture();
        fixture
            .vfs
            .write(&uri("nix://shell"), "{ description = \"env\"; }\n")
            .await
            .expect("write");
        let file = fixture
            .vfs
            .read(&uri("nix://shell"))
            .await
            .expect("read back");
        assert_eq!(file.content, "{ description = \"env\"; }\n");
        assert!(!file.meta.is_immutable());
        assert_eq!(file.meta.source_path, fixture.flake_path);

        let meta = fixture.vfs.stat(&uri("nix://shell")).await.expect("stat");
        assert_eq!(meta.kind, VfsKind::File);
        assert_eq!(meta.size, Some("{ description = \"env\"; }\n".len()));
    }

    #[tokio::test]
    async fn unknown_resource_lists_available() {
        let fixture = fixture();
        for input in ["nix://status", "nix://shell/extra", "nix://"] {
            let error = fixture.vfs.read(&uri(input)).await.unwrap_err();
            assert!(
                error.to_string().contains("nix://shell"),
                "{input}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn list_is_structurally_unsupported() {
        let fixture = fixture();
        let error = fixture.vfs.list(&uri("nix://shell")).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not support directory listing"),
            "{error}"
        );
    }

    #[test]
    fn completes_shell_only() {
        let fixture = fixture();
        assert!(fixture.vfs.capabilities().completion);
        let completions = fixture.vfs.complete("");
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].value, "shell");
        assert_eq!(fixture.vfs.complete("sh").len(), 1);
        assert!(fixture.vfs.complete("zzz").is_empty());
    }
}
