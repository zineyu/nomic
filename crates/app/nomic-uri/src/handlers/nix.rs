//! `nix://` 协议 handler：workspace 级 nix 环境定义（ADR-0041）。
//!
//! - `nix://shell` → `<workspace>/.nomic/flake.nix`（唯一资源），可读写：
//!   agent 经 write/edit 修改环境定义；bash 工具的 env 缓存按 mtime 失效，
//!   无需跨组件通知。
//! - 其余路径报 Resolve 错误并列出可用资源。
//! - `source_path` 始终为底层真实路径，grep/bash 据此与 fs 路径对齐。

use std::path::PathBuf;

use async_trait::async_trait;

use crate::handler::{ContentType, ProtocolHandler, UriError, UriResource, UrlCompletion};
use crate::parse::InternalUri;
use crate::root::WorkspaceRoot;

/// `nix://shell` handler：workspace `.nomic/flake.nix` 的可读写视图。
pub struct NixProtocolHandler {
    root: WorkspaceRoot,
}

impl std::fmt::Debug for NixProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NixProtocolHandler")
            .field("root", &self.root.snapshot())
            .finish()
    }
}

impl NixProtocolHandler {
    /// 以共享 workspace 根句柄构造。
    #[must_use]
    pub const fn new(root: WorkspaceRoot) -> Self {
        Self { root }
    }

    /// `nix://shell` → `<workspace>/.nomic/flake.nix`；其余路径报错。
    fn resolve_path(&self, url: &InternalUri) -> Result<PathBuf, UriError> {
        if url.raw_host != "shell" || !url.raw_path.is_empty() {
            return Err(UriError::Resolve(format!(
                "Unknown nix:// resource: {}. Available: nix://shell",
                url.without_query()
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

#[async_trait]
impl ProtocolHandler for NixProtocolHandler {
    fn scheme(&self) -> &'static str {
        "nix"
    }

    fn immutable(&self) -> bool {
        false
    }

    async fn resolve(&self, url: &InternalUri) -> Result<UriResource, UriError> {
        let path = self.resolve_path(url)?;
        let href = url.without_query();
        let content = tokio::fs::read_to_string(&path).await.map_err(|_| {
            UriError::Resolve(format!(
                "Could not resolve {href}: {} does not exist yet. \
                 Write nix://shell to create the workspace nix environment definition.",
                path.display()
            ))
        })?;
        let mut resource = UriResource::text(href, content);
        resource.content_type = ContentType::Plain;
        resource.source_path = Some(path);
        Ok(resource)
    }

    fn writable(&self) -> bool {
        true
    }

    async fn write(&self, url: &InternalUri, content: &str) -> Result<(), UriError> {
        let path = self.resolve_path(url)?;
        let href = url.without_query();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                UriError::Resolve(format!(
                    "Could not create parent directories for {href}: {error}"
                ))
            })?;
        }
        tokio::fs::write(&path, content)
            .await
            .map_err(|error| UriError::Resolve(format!("Could not write {href}: {error}")))
    }

    fn supports_completion(&self) -> bool {
        true
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::parse::parse_internal_uri;

    struct Fixture {
        _dir: TempDir,
        handler: NixProtocolHandler,
        flake_path: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().expect("temp dir");
        let flake_path = dir.path().join(".nomic").join("flake.nix");
        let root = WorkspaceRoot::new(Some(dir.path().to_path_buf()));
        Fixture {
            _dir: dir,
            handler: NixProtocolHandler::new(root),
            flake_path,
        }
    }

    fn uri(input: &str) -> InternalUri {
        parse_internal_uri(input).expect("parse")
    }

    #[tokio::test]
    async fn resolve_missing_flake_hints_write_to_create() {
        let fixture = fixture();
        let error = fixture
            .handler
            .resolve(&uri("nix://shell"))
            .await
            .unwrap_err();
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
            .handler
            .write(&uri("nix://shell"), "{ description = \"env\"; }\n")
            .await
            .expect("write");
        let resource = fixture
            .handler
            .resolve(&uri("nix://shell"))
            .await
            .expect("read back");
        assert_eq!(resource.content, "{ description = \"env\"; }\n");
        assert!(!resource.is_immutable());
        assert_eq!(
            resource.source_path.expect("source path"),
            fixture.flake_path
        );
    }

    #[tokio::test]
    async fn unknown_resource_lists_available() {
        let fixture = fixture();
        for input in ["nix://status", "nix://shell/extra", "nix://"] {
            let error = fixture.handler.resolve(&uri(input)).await.unwrap_err();
            assert!(
                error.to_string().contains("nix://shell"),
                "{input}: {error}"
            );
        }
    }

    #[test]
    fn completes_shell_only() {
        let fixture = fixture();
        assert!(fixture.handler.supports_completion());
        let completions = fixture.handler.complete("");
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].value, "shell");
        assert_eq!(fixture.handler.complete("sh").len(), 1);
        assert!(fixture.handler.complete("zzz").is_empty());
    }
}
