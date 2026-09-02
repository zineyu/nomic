//! `skill://` VFS（ADR-0040 §协议目录，ADR-0042 VFS 化）。
//!
//! 语义完全复用 [`SkillResolver::resolve_resource`]：host 段为 skill 名，
//! path 段为根目录内相对子路径（词法防穿越由 resolver 保证）；
//! 无子路径返回 `SKILL.md` 正文，目录返回渲染清单文档。
//! 只读：skill 是提示词资产，agent 不应直接改写（ADR-0040 §8.1）。

use async_trait::async_trait;
use nomic_skills::{SKILL_SCHEME, Skill, SkillResolver, SkillResource};

use crate::fs::{MAX_LISTING_ENTRIES, content_type_for, dir_entries};
use crate::parse::InternalUri;
use crate::vfs::{
    ContentType, UrlCompletion, Vfs, VfsCapabilities, VfsEntry, VfsError, VfsFile, VfsMetadata,
    render_listing,
};

/// `skill://<name>[/<path>]` VFS。
pub struct SkillVfs {
    resolver: SkillResolver,
}

impl std::fmt::Debug for SkillVfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillVfs").finish_non_exhaustive()
    }
}

impl SkillVfs {
    /// 以会话的 skill resolver 构造。
    #[must_use]
    pub const fn new(resolver: SkillResolver) -> Self {
        Self { resolver }
    }

    /// 解析 URI 目标为（skill 名，根内相对子路径）；空 / `"."` 子路径
    /// 退化为正文（`None`）。与 ADR-0040 时代 read.rs 的特判一致。
    fn target(uri: &InternalUri) -> Result<(&str, Option<&str>), VfsError> {
        let name = uri.raw_host.as_str();
        if name.is_empty() {
            return Err(VfsError::Resolve(format!(
                "Invalid {SKILL_SCHEME} URI: expected {SKILL_SCHEME}<name>[/<path>]"
            )));
        }
        let rel = match uri.raw_path.as_str() {
            "" | "." => None,
            rel => Some(rel),
        };
        Ok((name, rel))
    }

    /// 经 resolver 定位资源；错误附带原始 href 上下文。
    fn locate(&self, uri: &InternalUri) -> Result<SkillResource, VfsError> {
        let (name, rel) = Self::target(uri)?;
        self.resolver.resolve_resource(name, rel).map_err(|error| {
            VfsError::Resolve(format!("Could not resolve {}. {error}", uri.raw_href))
        })
    }
}

#[async_trait]
impl Vfs for SkillVfs {
    fn scheme(&self) -> &'static str {
        "skill"
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities::READ_ONLY_COMPLETION
    }

    async fn stat(&self, uri: &InternalUri) -> Result<VfsMetadata, VfsError> {
        match self.locate(uri)? {
            SkillResource::Instructions(skill) => {
                let mut meta = VfsMetadata::file(ContentType::Markdown, Some(skill.path.clone()));
                meta.size = Some(skill.document.body.len());
                Ok(meta)
            }
            SkillResource::File { path, .. } => {
                Ok(VfsMetadata::file(content_type_for(&path), Some(path)))
            }
            SkillResource::Directory { path, .. } => Ok(VfsMetadata::directory(Some(path))),
        }
    }

    async fn read(&self, uri: &InternalUri) -> Result<VfsFile, VfsError> {
        let (_, rel) = Self::target(uri)?;
        match self.locate(uri)? {
            SkillResource::Instructions(skill) => {
                let meta = VfsMetadata::file(ContentType::Markdown, Some(skill.path.clone()));
                let mut file =
                    VfsFile::text(uri.raw_href.clone(), skill.document.body.clone(), meta);
                file.details = Some(skill_details(&uri.raw_href, &skill, None));
                Ok(file)
            }
            SkillResource::File { skill, path } => {
                let content = tokio::fs::read_to_string(&path).await.map_err(|error| {
                    VfsError::Resolve(format!("Could not resolve {}. {error}", uri.raw_href))
                })?;
                let meta = VfsMetadata::file(content_type_for(&path), Some(path));
                let mut file = VfsFile::text(uri.raw_href.clone(), content, meta);
                file.details = Some(skill_details(&uri.raw_href, &skill, rel));
                Ok(file)
            }
            SkillResource::Directory { skill, path } => {
                let entries = dir_entries(&path, MAX_LISTING_ENTRIES)
                    .await
                    .map_err(|error| {
                        VfsError::Resolve(format!("Could not resolve {}. {error}", uri.raw_href))
                    })?;
                let meta = VfsMetadata::directory(Some(path));
                let mut file = VfsFile::text(uri.raw_href.clone(), render_listing(&entries), meta);
                // 目录清单的 resource 标注带尾随 `/`（ADR-0040 时代 read.rs 的既有契约）
                let listed = rel.map(|r| format!("{}/", r.trim_end_matches('/')));
                file.details = Some(skill_details(&uri.raw_href, &skill, listed.as_deref()));
                Ok(file)
            }
        }
    }

    async fn list(&self, uri: &InternalUri) -> Result<Vec<VfsEntry>, VfsError> {
        let (name, rel) = Self::target(uri)?;
        // 根（`skill://<name>`）经 resolver 退化为正文资源，但对 list 而言
        // 应视作 skill 根目录。
        if rel.is_none() {
            let skill = self.resolver.resolve(name).map_err(|error| {
                VfsError::Resolve(format!("Could not resolve {}. {error}", uri.raw_href))
            })?;
            return dir_entries(&skill.root, MAX_LISTING_ENTRIES)
                .await
                .map_err(|error| {
                    VfsError::Resolve(format!("Could not resolve {}. {error}", uri.raw_href))
                });
        }
        match self.locate(uri)? {
            SkillResource::Directory { path, .. } => dir_entries(&path, MAX_LISTING_ENTRIES)
                .await
                .map_err(|error| {
                    VfsError::Resolve(format!("Could not resolve {}. {error}", uri.raw_href))
                }),
            _ => Err(VfsError::Resolve(format!(
                "{} is a file, not a directory.",
                uri.raw_href
            ))),
        }
    }

    fn complete(&self, _query: &str) -> Vec<UrlCompletion> {
        self.resolver
            .catalog()
            .iter()
            .filter(|skill| skill.document.enabled)
            .map(|skill| UrlCompletion {
                value: skill.name.clone(),
                label: None,
                description: Some(skill.document.description.clone()),
            })
            .collect()
    }
}

/// `details.source` 的 skill 标注（与 ADR-0040 时代 read.rs 的字段形状一致）。
fn skill_details(uri: &str, skill: &Skill, resource: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "source": {
            "kind": "skill",
            "uri": uri,
            "name": skill.name,
            "scope": skill.scope.to_string(),
            "path": skill.path.display().to_string(),
            "resource": resource,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_internal_uri;
    use crate::vfs::VfsKind;
    use nomic_skills::{ProjectDiscovery, SkillRoot, SkillScope};
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        vfs: SkillVfs,
    }

    fn fixture() -> Fixture {
        let dir = TempDir::new().expect("temp dir");
        let skills_dir = dir.path().join("skills");
        let demo = skills_dir.join("demo");
        std::fs::create_dir_all(demo.join("scripts")).expect("mkdir");
        std::fs::write(
            demo.join("SKILL.md"),
            "---\ndescription: Demo skill\n---\ndemo body",
        )
        .expect("write");
        std::fs::write(demo.join("scripts/run.sh"), "echo one\n").expect("write");
        let resolver = SkillResolver::new(
            dir.path(),
            ProjectDiscovery::Roots(Vec::new()),
            vec![SkillRoot {
                path: skills_dir,
                scope: SkillScope::Project,
            }],
        )
        .expect("resolver");
        Fixture {
            _dir: dir,
            vfs: SkillVfs::new(resolver),
        }
    }

    fn uri(input: &str) -> InternalUri {
        parse_internal_uri(input).expect("parse")
    }

    #[tokio::test]
    async fn reads_instructions_file_and_directory() {
        let fixture = fixture();
        let vfs = &fixture.vfs;

        let file = vfs.read(&uri("skill://demo")).await.expect("instructions");
        assert_eq!(file.content, "demo body");
        assert_eq!(file.meta.content_type, ContentType::Markdown);
        assert!(file.meta.source_path.expect("path").ends_with("SKILL.md"));
        assert_eq!(
            file.details.as_ref().expect("details")["source"]["name"].as_str(),
            Some("demo")
        );

        let file = vfs
            .read(&uri("skill://demo/scripts/run.sh"))
            .await
            .expect("file");
        assert_eq!(file.content, "echo one\n");
        assert_eq!(
            file.details.as_ref().expect("details")["source"]["resource"].as_str(),
            Some("scripts/run.sh")
        );

        let file = vfs
            .read(&uri("skill://demo/scripts"))
            .await
            .expect("directory");
        assert_eq!(file.content, "run.sh");
        assert_eq!(file.meta.kind, VfsKind::Directory);
        assert_eq!(
            file.details.as_ref().expect("details")["source"]["resource"].as_str(),
            Some("scripts/")
        );

        // 空 / 点号子路径退化为正文
        for input in ["skill://demo/", "skill://demo/."] {
            let file = vfs.read(&uri(input)).await.expect("degenerate");
            assert_eq!(file.content, "demo body", "{input}");
        }
    }

    #[tokio::test]
    async fn stat_reports_kind_without_content() {
        let fixture = fixture();
        let vfs = &fixture.vfs;

        let meta = vfs.stat(&uri("skill://demo")).await.expect("instructions");
        assert_eq!(meta.kind, VfsKind::File);
        assert_eq!(meta.content_type, ContentType::Markdown);
        assert_eq!(meta.size, Some("demo body".len()));

        let meta = vfs.stat(&uri("skill://demo/scripts")).await.expect("dir");
        assert_eq!(meta.kind, VfsKind::Directory);

        let meta = vfs
            .stat(&uri("skill://demo/scripts/run.sh"))
            .await
            .expect("file");
        assert_eq!(meta.kind, VfsKind::File);
        assert_eq!(meta.content_type, ContentType::Plain);
    }

    #[tokio::test]
    async fn list_returns_typed_entries() {
        let fixture = fixture();
        let mut entries = fixture
            .vfs
            .list(&uri("skill://demo"))
            .await
            .expect("list root");
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(
            entries,
            vec![
                VfsEntry {
                    name: "SKILL.md".into(),
                    kind: VfsKind::File,
                },
                VfsEntry {
                    name: "scripts".into(),
                    kind: VfsKind::Directory,
                },
            ]
        );

        // 文件目标不可 list
        let error = fixture
            .vfs
            .list(&uri("skill://demo/scripts/run.sh"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not a directory"), "{error}");
    }

    #[tokio::test]
    async fn errors_carry_uri_context() {
        let fixture = fixture();
        let error = fixture.vfs.read(&uri("skill://missing")).await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("skill://missing"), "{message}");
        assert!(message.contains("available: demo"), "{message}");

        let error = fixture.vfs.read(&uri("skill://")).await.unwrap_err();
        assert!(
            error.to_string().contains("expected skill://<name>"),
            "{error}"
        );
    }

    #[test]
    fn completes_enabled_skill_names() {
        let fixture = fixture();
        assert!(fixture.vfs.capabilities().completion);
        let completions = fixture.vfs.complete("");
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].value, "demo");
        assert_eq!(completions[0].description.as_deref(), Some("Demo skill"));
    }
}
