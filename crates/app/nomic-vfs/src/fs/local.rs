//! `local://` VFS：会话 workspace 内的文件/目录（ADR-0040 §协议目录，
//! ADR-0042 VFS 化）。
//!
//! - `local://<path>` 以 workspace 根为基准；路径经词法规范化，越出根即拒绝
//!   （`..` 穿越、绝对路径、`~` 不展开——与 oh-my-pi 的 local 安全边界一致）。
//! - `local://`（空路径）列根目录清单。
//! - 可写：write/edit 经 router 分发到这里；目录清单是派生内容（router 盖
//!   不可变章）。
//! - `source_path` 始终为底层真实路径，grep/bash 据此与 fs 路径对齐。

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;

use crate::fs::{MAX_LISTING_ENTRIES, content_type_for, dir_entries};
use crate::parse::{InternalUri, percent_decode};
use crate::root::WorkspaceRoot;
use crate::vfs::{
    UrlCompletion, Vfs, VfsCapabilities, VfsEntry, VfsError, VfsFile, VfsMetadata, render_listing,
};

/// `local://<path>` VFS。
pub struct LocalVfs {
    root: WorkspaceRoot,
}

impl std::fmt::Debug for LocalVfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalVfs")
            .field("root", &self.root.snapshot())
            .finish()
    }
}

impl LocalVfs {
    /// 以共享 workspace 根句柄构造。
    #[must_use]
    pub const fn new(root: WorkspaceRoot) -> Self {
        Self { root }
    }

    /// 解析 `local://` 目标到 workspace 内绝对路径；空路径 = 根本身。
    fn resolve_path(&self, uri: &InternalUri) -> Result<PathBuf, VfsError> {
        // raw_path 不含前导 `/`：host 与 path 段之间手动补分隔
        let rel = if uri.raw_path.is_empty() {
            uri.raw_host.clone()
        } else {
            format!("{}/{}", uri.raw_host, uri.raw_path)
        };
        let rel = percent_decode(&rel);
        let root = self
            .root
            .snapshot()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        normalize_within(&root, &rel).ok_or_else(|| {
            VfsError::Resolve(format!(
                "Invalid local:// path: {rel} escapes the workspace root. \
             Paths under local:// must stay inside the session workspace."
            ))
        })
    }
}

#[async_trait]
impl Vfs for LocalVfs {
    fn scheme(&self) -> &'static str {
        "local"
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities::READ_WRITE_COMPLETION
    }

    async fn stat(&self, uri: &InternalUri) -> Result<VfsMetadata, VfsError> {
        let path = self.resolve_path(uri)?;
        let href = uri.without_query();
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))?;
        if metadata.is_dir() {
            return Ok(VfsMetadata::directory(Some(path)));
        }
        let mut meta = VfsMetadata::file(content_type_for(&path), Some(path));
        meta.size = usize::try_from(metadata.len()).ok();
        Ok(meta)
    }

    async fn read(&self, uri: &InternalUri) -> Result<VfsFile, VfsError> {
        let path = self.resolve_path(uri)?;
        let href = uri.without_query();
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))?;
        if metadata.is_dir() {
            let entries = dir_entries(&path, MAX_LISTING_ENTRIES)
                .await
                .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))?;
            return Ok(VfsFile::text(
                href,
                render_listing(&entries),
                VfsMetadata::directory(Some(path)),
            ));
        }
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))?;
        let mut meta = VfsMetadata::file(content_type_for(&path), Some(path));
        meta.size = Some(content.len());
        Ok(VfsFile::text(href, content, meta))
    }

    async fn list(&self, uri: &InternalUri) -> Result<Vec<VfsEntry>, VfsError> {
        let path = self.resolve_path(uri)?;
        let href = uri.without_query();
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))?;
        if !metadata.is_dir() {
            return Err(VfsError::Resolve(format!("{href} is a file, not a directory.")));
        }
        dir_entries(&path, MAX_LISTING_ENTRIES)
            .await
            .map_err(|error| VfsError::Resolve(format!("Could not resolve {href}. {error}")))
    }

    async fn write(&self, uri: &InternalUri, content: &str) -> Result<(), VfsError> {
        let path = self.resolve_path(uri)?;
        let href = uri.without_query();
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
        let Some(root) = self
            .root
            .snapshot()
            .or_else(|| std::env::current_dir().ok())
        else {
            return Vec::new();
        };
        complete_paths(&root, query, MAX_LISTING_ENTRIES)
    }
}

/// 词法规范化 `root.join(rel)`，越出 `root` 返回 `None`；空路径（或只有
/// `.`）返回根本身。只处理 `Normal` / `CurDir` / `ParentDir` 组件：拒绝
/// 绝对路径与 Windows 前缀。不触碰文件系统。
fn normalize_within(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    let mut depth = 0usize;
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => {
                path.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                path.pop();
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(path)
}

/// workspace 内路径补全：两层内的文件/目录（目录带 `/`），按前缀过滤。
fn complete_paths(root: &Path, query: &str, cap: usize) -> Vec<UrlCompletion> {
    let mut out = Vec::new();
    let mut frontier: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    let mut depth = 0;
    while !frontier.is_empty() && depth < 3 && out.len() < cap {
        let mut next = Vec::new();
        for (dir, prefix) in frontier {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if out.len() >= cap {
                    break;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue; // 隐藏文件不进补全
                }
                let rel = format!("{prefix}{name}");
                let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
                let value = if is_dir {
                    format!("{rel}/")
                } else {
                    rel.clone()
                };
                if value.starts_with(query) {
                    out.push(UrlCompletion {
                        value: value.clone(),
                        label: None,
                        description: None,
                    });
                }
                if is_dir && (value.starts_with(query) || query.starts_with(&value)) {
                    next.push((entry.path(), format!("{rel}/")));
                }
            }
        }
        frontier = next;
        depth += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_internal_uri;
    use crate::vfs::{ContentType, VfsKind};
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        root: WorkspaceRoot,
        vfs: LocalVfs,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("README.md"), "# Demo\nline 2\n").expect("write");
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").expect("write");
        let root = WorkspaceRoot::new(Some(dir.path().to_path_buf()));
        Fixture {
            _dir: dir,
            root: root.clone(),
            vfs: LocalVfs::new(root),
        }
    }

    fn uri(input: &str) -> InternalUri {
        parse_internal_uri(input).expect("parse")
    }

    #[tokio::test]
    async fn reads_files_dirs_and_root() {
        let fixture = fixture();
        let vfs = &fixture.vfs;

        let file = vfs.read(&uri("local://README.md")).await.expect("file");
        assert_eq!(file.content, "# Demo\nline 2\n");
        assert_eq!(file.meta.content_type, ContentType::Markdown);
        assert!(!file.meta.is_immutable());
        assert!(file.meta.source_path.expect("path").ends_with("README.md"));

        let file = vfs.read(&uri("local://src")).await.expect("dir");
        assert_eq!(file.content, "main.rs");
        assert_eq!(file.meta.kind, VfsKind::Directory);
        // 目录清单的不可变章由 router 统一盖；VFS 自身不设置
        assert!(file.meta.immutable.is_none());

        let file = vfs.read(&uri("local://")).await.expect("root");
        assert_eq!(file.meta.kind, VfsKind::Directory);
        assert!(file.content.contains("src/"));
        assert!(file.content.contains("README.md"));
    }

    #[tokio::test]
    async fn stat_reports_metadata_without_content() {
        let fixture = fixture();
        let vfs = &fixture.vfs;

        let meta = vfs.stat(&uri("local://README.md")).await.expect("file");
        assert_eq!(meta.kind, VfsKind::File);
        assert_eq!(meta.content_type, ContentType::Markdown);
        assert_eq!(meta.size, Some("# Demo\nline 2\n".len()));

        let meta = vfs.stat(&uri("local://src")).await.expect("dir");
        assert_eq!(meta.kind, VfsKind::Directory);
        assert_eq!(meta.size, None);
    }

    #[tokio::test]
    async fn list_returns_typed_entries() {
        let fixture = fixture();
        let mut entries = fixture.vfs.list(&uri("local://")).await.expect("list");
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(
            entries,
            vec![
                VfsEntry {
                    name: "README.md".into(),
                    kind: VfsKind::File,
                },
                VfsEntry {
                    name: "src".into(),
                    kind: VfsKind::Directory,
                },
            ]
        );

        // 文件目标不可 list
        let error = fixture
            .vfs
            .list(&uri("local://README.md"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not a directory"), "{error}");
    }

    #[tokio::test]
    async fn rejects_traversal_and_missing() {
        let fixture = fixture();
        let vfs = &fixture.vfs;
        for input in [
            "local://../escape",
            "local://src/../../escape",
            "local://./..",
        ] {
            let error = vfs.stat(&uri(input)).await.unwrap_err();
            assert!(
                error.to_string().contains("escapes the workspace root"),
                "{input}: {error}"
            );
        }
        let error = vfs.read(&uri("local://missing.txt")).await.unwrap_err();
        assert!(error.to_string().contains("local://missing.txt"), "{error}");
    }

    #[tokio::test]
    async fn write_creates_parents_and_roundtrips() {
        let fixture = fixture();
        let vfs = &fixture.vfs;
        vfs.write(&uri("local://notes/deep/new.md"), "hello")
            .await
            .expect("write");
        let file = vfs
            .read(&uri("local://notes/deep/new.md"))
            .await
            .expect("read back");
        assert_eq!(file.content, "hello");
        assert_eq!(
            std::fs::read_to_string(
                fixture
                    .root
                    .snapshot()
                    .expect("root")
                    .join("notes/deep/new.md")
            )
            .expect("fs read"),
            "hello"
        );
        // 目录目标不可写
        let error = vfs.write(&uri("local://src"), "x").await.unwrap_err();
        assert!(error.to_string().contains("Could not write"), "{error}");
    }

    #[test]
    fn completes_workspace_paths() {
        let fixture = fixture();
        assert!(fixture.vfs.capabilities().completion);
        let completions = fixture.vfs.complete("");
        let values: Vec<&str> = completions.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"README.md"), "{values:?}");
        assert!(values.contains(&"src/"), "{values:?}");
        let completions = fixture.vfs.complete("src/");
        let nested: Vec<&str> = completions.iter().map(|c| c.value.as_str()).collect();
        assert!(nested.contains(&"src/main.rs"), "{nested:?}");
    }
}
