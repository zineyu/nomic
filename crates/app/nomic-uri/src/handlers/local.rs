//! `local://` 协议 handler：会话 workspace 内的文件/目录（ADR-0040 §协议目录）。
//!
//! - `local://<path>` 以 workspace 根为基准；路径经词法规范化，越出根即拒绝
//!   （`..` 穿越、绝对路径、`~` 不展开——与 oh-my-pi 的 local 安全边界一致）。
//! - `local://`（空路径）列根目录清单。
//! - 可写：write/edit 经 router 分发到这里；目录清单是派生内容（盖不可变章）。
//! - `source_path` 始终为底层真实路径，grep/bash 据此与 fs 路径对齐（T12）。

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;

use crate::handler::{ContentType, ProtocolHandler, UriError, UriResource, UrlCompletion};
use crate::parse::{InternalUri, percent_decode};
use crate::root::WorkspaceRoot;

/// 目录清单 / 补全的条目数上限。
const MAX_LISTING_ENTRIES: usize = 1000;

/// `local://<path>` handler。
pub struct LocalProtocolHandler {
    root: WorkspaceRoot,
}

impl std::fmt::Debug for LocalProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalProtocolHandler")
            .field("root", &self.root.snapshot())
            .finish()
    }
}

impl LocalProtocolHandler {
    /// 以共享 workspace 根句柄构造。
    #[must_use]
    pub const fn new(root: WorkspaceRoot) -> Self {
        Self { root }
    }

    /// 解析 `local://` 目标到 workspace 内绝对路径；空路径 = 根本身。
    fn resolve_path(&self, url: &InternalUri) -> Result<PathBuf, UriError> {
        // raw_path 不含前导 `/`：host 与 path 段之间手动补分隔
        let rel = if url.raw_path.is_empty() {
            url.raw_host.clone()
        } else {
            format!("{}/{}", url.raw_host, url.raw_path)
        };
        let rel = percent_decode(&rel);
        let root = self
            .root
            .snapshot()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        normalize_within(&root, &rel).ok_or_else(|| {
            UriError::Resolve(format!(
                "Invalid local:// path: {rel} escapes the workspace root. \
             Paths under local:// must stay inside the session workspace."
            ))
        })
    }
}

#[async_trait]
impl ProtocolHandler for LocalProtocolHandler {
    fn scheme(&self) -> &'static str {
        "local"
    }

    fn immutable(&self) -> bool {
        false
    }

    async fn resolve(&self, url: &InternalUri) -> Result<UriResource, UriError> {
        let path = self.resolve_path(url)?;
        let href = url.without_query();
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|error| UriError::Resolve(format!("Could not resolve {href}. {error}")))?;
        if metadata.is_dir() {
            let listing = dir_listing(&path, MAX_LISTING_ENTRIES)
                .await
                .map_err(|error| UriError::Resolve(format!("Could not resolve {href}. {error}")))?;
            let mut resource = UriResource::text(href, listing);
            resource.is_directory = true;
            resource.immutable = Some(true); // 目录清单是派生内容
            resource.source_path = Some(path);
            return Ok(resource);
        }
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| UriError::Resolve(format!("Could not resolve {href}. {error}")))?;
        let mut resource = UriResource::text(href, content);
        resource.content_type = content_type_for(&path);
        resource.source_path = Some(path);
        Ok(resource)
    }

    fn writable(&self) -> bool {
        true
    }

    async fn write(&self, url: &InternalUri, content: &str) -> Result<(), UriError> {
        let path = self.resolve_path(url)?;
        let href = url.without_query();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                UriError::Resolve(format!(
                    "Could not create parent directories for {href}: {error}"
                ))
            })?;
        }
        tokio::fs::write(&path, content)
            .await
            .map_err(|error| UriError::Resolve(format!("Could not write {href}. {error}")))
    }

    fn supports_completion(&self) -> bool {
        true
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

/// 目录清单：一行一个条目，目录以 `/` 结尾，按名称排序（目录在前）。
async fn dir_listing(path: &Path, cap: usize) -> std::io::Result<String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
        if dirs.len() + files.len() >= cap {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().await?.is_dir() {
            dirs.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }
    dirs.sort();
    files.sort();
    Ok(dirs.into_iter().chain(files).collect::<Vec<_>>().join("\n"))
}

/// 按扩展名推断内容类别。
fn content_type_for(path: &Path) -> ContentType {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md" | "markdown") => ContentType::Markdown,
        Some("json") => ContentType::Json,
        _ => ContentType::Plain,
    }
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
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        root: WorkspaceRoot,
        handler: LocalProtocolHandler,
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
            handler: LocalProtocolHandler::new(root),
        }
    }

    fn uri(input: &str) -> InternalUri {
        crate::parse::parse_internal_uri(input).expect("parse")
    }

    #[tokio::test]
    async fn resolves_files_dirs_and_root() {
        let fixture = fixture();
        let handler = &fixture.handler;

        let resource = handler
            .resolve(&uri("local://README.md"))
            .await
            .expect("file");
        assert_eq!(resource.content, "# Demo\nline 2\n");
        assert_eq!(resource.content_type, ContentType::Markdown);
        assert!(!resource.is_immutable());
        assert!(resource.source_path.expect("path").ends_with("README.md"));

        let resource = handler.resolve(&uri("local://src")).await.expect("dir");
        assert_eq!(resource.content, "main.rs");
        assert!(resource.is_directory);
        assert!(resource.is_immutable()); // 目录清单派生内容

        let resource = handler.resolve(&uri("local://")).await.expect("root");
        assert!(resource.is_directory);
        assert!(resource.content.contains("src/"));
        assert!(resource.content.contains("README.md"));
    }

    #[tokio::test]
    async fn rejects_traversal_and_missing() {
        let fixture = fixture();
        let handler = &fixture.handler;
        for input in [
            "local://../escape",
            "local://src/../../escape",
            "local://./..",
        ] {
            let error = handler.resolve(&uri(input)).await.unwrap_err();
            assert!(
                error.to_string().contains("escapes the workspace root"),
                "{input}: {error}"
            );
        }
        let error = handler
            .resolve(&uri("local://missing.txt"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("local://missing.txt"), "{error}");
    }

    #[tokio::test]
    async fn write_creates_parents_and_roundtrips() {
        let fixture = fixture();
        let handler = &fixture.handler;
        handler
            .write(&uri("local://notes/deep/new.md"), "hello")
            .await
            .expect("write");
        let resource = handler
            .resolve(&uri("local://notes/deep/new.md"))
            .await
            .expect("read back");
        assert_eq!(resource.content, "hello");
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
        let error = handler.write(&uri("local://src"), "x").await.unwrap_err();
        assert!(error.to_string().contains("Could not write"), "{error}");
    }

    #[test]
    fn completes_workspace_paths() {
        let fixture = fixture();
        assert!(fixture.handler.supports_completion());
        let completions = fixture.handler.complete("");
        let values: Vec<&str> = completions.iter().map(|c| c.value.as_str()).collect();
        assert!(values.contains(&"README.md"), "{values:?}");
        assert!(values.contains(&"src/"), "{values:?}");
        let completions = fixture.handler.complete("src/");
        let nested: Vec<&str> = completions.iter().map(|c| c.value.as_str()).collect();
        assert!(nested.contains(&"src/main.rs"), "{nested:?}");
    }
}
