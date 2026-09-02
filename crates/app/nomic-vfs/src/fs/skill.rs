//! `skill://` 挂载：只读提示词资产（ADR-0040 §协议目录，ADR-0042 VFS 化，
//! ADR-0043 目录化挂载）。
//!
//! 语义复用 [`SkillResolver`]：host 段为 skill 名（查表定位 backing
//! root），path 段为根目录内相对子路径（词法防穿越由 resolver 保证）；
//! 根经 `index` 钩子映射到 SKILL.md——read/stat 以索引文件代表目录，
//! list 仍列根目录条目。`transform` 钩子把根读出的内容替换为去
//! frontmatter 的正文并附加 `details.source` 标注。
//! 只读：skill 是提示词资产，agent 不应直接改写（ADR-0040 §8.1）。

use std::path::PathBuf;

use nomic_skills::{SKILL_SCHEME, Skill, SkillResolver, SkillResource};

use crate::mount::{DirMount, Mount};
use crate::parse::InternalUri;
use crate::vfs::{UrlCompletion, VfsCapabilities, VfsError, VfsFile, VfsKind};

/// `skill://` 的挂载声明。
pub struct SkillMount {
    resolver: SkillResolver,
}

impl std::fmt::Debug for SkillMount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillMount").finish_non_exhaustive()
    }
}

impl SkillMount {
    /// 以会话的 skill resolver 构造。
    #[must_use]
    pub const fn new(resolver: SkillResolver) -> Self {
        Self { resolver }
    }

    /// 解析 URI 目标为（skill 名，根内相对子路径）；空 / `"."` 子路径
    /// 退化为根（`None`）。
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
}

impl Mount for SkillMount {
    fn scheme(&self) -> &'static str {
        "skill"
    }

    fn capabilities(&self) -> VfsCapabilities {
        VfsCapabilities::READ_ONLY_COMPLETION
    }

    fn locate(&self, uri: &InternalUri) -> Result<PathBuf, VfsError> {
        let (name, rel) = Self::target(uri)?;
        let resolve_error = |error: nomic_skills::SkillsError| {
            VfsError::Resolve(format!("Could not resolve {}. {error}", uri.raw_href))
        };
        match rel {
            // 根：backing 路径为 skill 根目录；read/stat 经 index 钩子
            // 展开到 SKILL.md，list 列根目录条目
            None => self
                .resolver
                .resolve(name)
                .map(|skill| skill.root)
                .map_err(resolve_error),
            Some(rel) => match self
                .resolver
                .resolve_resource(name, Some(rel))
                .map_err(resolve_error)?
            {
                SkillResource::File { path, .. } | SkillResource::Directory { path, .. } => {
                    Ok(path)
                }
                // rel 非空时 resolver 不返回 Instructions；防御性落到根目录
                SkillResource::Instructions(skill) => Ok(skill.root),
            },
        }
    }

    fn index(&self, uri: &InternalUri) -> Option<&'static str> {
        // 索引仅对根生效：子目录即使含 SKILL.md 也仍是普通清单
        match Self::target(uri) {
            Ok((_, None)) => Some("SKILL.md"),
            _ => None,
        }
    }

    fn transform(&self, uri: &InternalUri, file: VfsFile) -> VfsFile {
        let Ok((name, rel)) = Self::target(uri) else {
            return file;
        };
        let Ok(skill) = self.resolver.resolve(name) else {
            return file;
        };
        let mut file = file;
        let resource = match (rel, file.meta.kind) {
            // 根（索引文件）：内容替换为去 frontmatter 的正文
            (None, _) => {
                file.content.clone_from(&skill.document.body);
                None
            }
            // 目录清单的 resource 标注带尾随 `/`
            (Some(rel), VfsKind::Directory) => Some(format!("{}/", rel.trim_end_matches('/'))),
            (Some(rel), VfsKind::File) => Some(rel.to_string()),
        };
        file.details = Some(skill_details(&uri.raw_href, &skill, resource.as_deref()));
        file
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

/// `skill://<name>[/<path>]` VFS：skill 根目录的只读目录化挂载
///（ADR-0043）。
pub type SkillVfs = DirMount<SkillMount>;

impl DirMount<SkillMount> {
    /// 以会话的 skill resolver 构造。
    #[must_use]
    pub const fn new(resolver: SkillResolver) -> Self {
        Self::from_decl(SkillMount::new(resolver))
    }
}

/// `details.source` 的 skill 标注。
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
    use crate::vfs::{ContentType, Vfs, VfsEntry};
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
        assert!(file.meta.source_path.ends_with("SKILL.md"));
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
    async fn stat_reports_backing_file_facts() {
        let fixture = fixture();
        let vfs = &fixture.vfs;

        // 根经索引展开为 SKILL.md；stat 报告背书文件的事实（不应用
        // 内容变换——size 为原始文件字节数，ADR-0043 §语义对齐）
        let meta = vfs.stat(&uri("skill://demo")).await.expect("instructions");
        assert_eq!(meta.kind, VfsKind::File);
        assert_eq!(meta.content_type, ContentType::Markdown);
        assert_eq!(
            meta.size,
            Some("---\ndescription: Demo skill\n---\ndemo body".len())
        );

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
