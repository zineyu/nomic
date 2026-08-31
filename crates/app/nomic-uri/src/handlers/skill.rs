//! `skill://` 协议 handler（ADR-0040 §协议目录）。
//!
//! 语义完全复用 [`SkillResolver::resolve_resource`]：host 段为 skill 名，
//! path 段为根目录内相对子路径（词法防穿越由 resolver 保证）；
//! 无子路径返回 `SKILL.md` 正文，目录返回一行一条目的清单。
//! 只读：skill 是提示词资产，agent 不应直接改写（ADR-0040 §8.1）。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use nomic_skills::{SKILL_SCHEME, Skill, SkillResolver, SkillResource};

use crate::handler::{ContentType, ProtocolHandler, UriError, UriResource, UrlCompletion};
use crate::parse::InternalUri;

/// `skill://<name>[/<path>]` handler。
pub struct SkillProtocolHandler {
    resolver: SkillResolver,
}

impl std::fmt::Debug for SkillProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillProtocolHandler")
            .finish_non_exhaustive()
    }
}

impl SkillProtocolHandler {
    /// 以会话的 skill resolver 构造。
    #[must_use]
    pub const fn new(resolver: SkillResolver) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl ProtocolHandler for SkillProtocolHandler {
    fn scheme(&self) -> &'static str {
        "skill"
    }

    fn immutable(&self) -> bool {
        true
    }

    async fn resolve(&self, url: &InternalUri) -> Result<UriResource, UriError> {
        let name = &url.raw_host;
        if name.is_empty() {
            return Err(UriError::Resolve(format!(
                "Invalid {SKILL_SCHEME} URI: expected {SKILL_SCHEME}<name>[/<path>]"
            )));
        }
        // 与迁移前 read.rs 的特判一致：空 / "." 子路径退化为正文
        let rel = match url.raw_path.as_str() {
            "" | "." => None,
            rel => Some(rel),
        };
        let resource = self.resolver.resolve_resource(name, rel).map_err(|error| {
            UriError::Resolve(format!("Could not resolve {}. {error}", url.raw_href))
        })?;
        match resource {
            SkillResource::Instructions(skill) => {
                let mut resource =
                    UriResource::text(url.raw_href.clone(), skill.document.body.clone());
                resource.content_type = ContentType::Markdown;
                resource.source_path = Some(skill.path.clone());
                resource.details = Some(skill_details(&url.raw_href, &skill, None));
                Ok(resource)
            }
            SkillResource::File { skill, path } => {
                let content = tokio::fs::read_to_string(&path).await.map_err(|error| {
                    UriError::Resolve(format!("Could not resolve {}. {error}", url.raw_href))
                })?;
                let mut resource = UriResource::text(url.raw_href.clone(), content);
                resource.content_type = content_type_for(&path);
                resource.source_path = Some(path);
                resource.details = Some(skill_details(&url.raw_href, &skill, rel));
                Ok(resource)
            }
            SkillResource::Directory { skill, path } => {
                let listing = read_dir_listing(&path).await.map_err(|error| {
                    UriError::Resolve(format!("Could not resolve {}. {error}", url.raw_href))
                })?;
                let mut resource = UriResource::text(url.raw_href.clone(), listing);
                resource.is_directory = true;
                // 目录清单是派生内容：盖不可变章（此处与 handler 默认一致，显式表达意图）
                resource.immutable = Some(true);
                resource.source_path = Some(path);
                // 目录清单的 resource 标注带尾随 `/`（迁移前 read.rs 的既有契约）
                let listed = rel.map(|r| format!("{}/", r.trim_end_matches('/')));
                resource.details = Some(skill_details(&url.raw_href, &skill, listed.as_deref()));
                Ok(resource)
            }
        }
    }

    fn supports_completion(&self) -> bool {
        true
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

/// `details.source` 的 skill 标注（与迁移前 read.rs 的字段形状一致）。
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

/// 目录清单：一行一个条目，目录以 `/` 结尾，按名称排序（目录在前）。
async fn read_dir_listing(path: &Path) -> std::io::Result<String> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut entries = tokio::fs::read_dir(path).await?;
    while let Some(entry) = entries.next_entry().await? {
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

/// 供 read 工具的 sed 续读提示使用的路径占位（虚拟资源无底层路径时）。
#[allow(dead_code)]
fn fallback_hint_path(url: &InternalUri) -> PathBuf {
    PathBuf::from(&url.raw_href)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomic_skills::{ProjectDiscovery, SkillRoot, SkillScope};
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        handler: SkillProtocolHandler,
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
            handler: SkillProtocolHandler::new(resolver),
        }
    }

    fn uri(input: &str) -> InternalUri {
        crate::parse::parse_internal_uri(input).expect("parse")
    }

    #[tokio::test]
    async fn resolves_instructions_file_and_directory() {
        let fixture = fixture();
        let handler = &fixture.handler;

        let resource = handler
            .resolve(&uri("skill://demo"))
            .await
            .expect("instructions");
        assert_eq!(resource.content, "demo body");
        assert_eq!(resource.content_type, ContentType::Markdown);
        assert!(resource.source_path.expect("path").ends_with("SKILL.md"));
        assert_eq!(
            resource.details.as_ref().expect("details")["source"]["name"].as_str(),
            Some("demo")
        );

        let resource = handler
            .resolve(&uri("skill://demo/scripts/run.sh"))
            .await
            .expect("file");
        assert_eq!(resource.content, "echo one\n");
        assert_eq!(
            resource.details.as_ref().expect("details")["source"]["resource"].as_str(),
            Some("scripts/run.sh")
        );

        let resource = handler
            .resolve(&uri("skill://demo/scripts"))
            .await
            .expect("directory");
        assert_eq!(resource.content, "run.sh");
        assert!(resource.is_directory);
        assert_eq!(
            resource.details.as_ref().expect("details")["source"]["resource"].as_str(),
            Some("scripts/")
        );

        // 空 / 点号子路径退化为正文
        for input in ["skill://demo/", "skill://demo/."] {
            let resource = handler.resolve(&uri(input)).await.expect("degenerate");
            assert_eq!(resource.content, "demo body", "{input}");
        }
    }

    #[tokio::test]
    async fn errors_carry_uri_context() {
        let fixture = fixture();
        let error = fixture
            .handler
            .resolve(&uri("skill://missing"))
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("skill://missing"), "{message}");
        assert!(message.contains("available: demo"), "{message}");

        let error = fixture.handler.resolve(&uri("skill://")).await.unwrap_err();
        assert!(
            error.to_string().contains("expected skill://<name>"),
            "{error}"
        );
    }

    #[test]
    fn completes_enabled_skill_names() {
        let fixture = fixture();
        assert!(fixture.handler.supports_completion());
        let completions = fixture.handler.complete("");
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].value, "demo");
        assert_eq!(completions[0].description.as_deref(), Some("Demo skill"));
    }
}
