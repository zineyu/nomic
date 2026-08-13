//! `read` 工具：文件 / `skill://` 读取、offset/limit、头部截断与翻页提示。

use std::path::Path;

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use nomic_skills::{SKILL_SCHEME, SkillResolver, SkillResource, SkillsError};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::truncate::{
    Continuation, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, exceeds_notice, truncate_head,
};

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Path to the file to read (relative or absolute), or skill://<name>[/<path>]
    pub path: String,
    /// Line number to start reading from (1-indexed)
    pub offset: Option<usize>,
    /// Maximum number of lines to read
    pub limit: Option<usize>,
}

/// `read` 工具。
#[derive(Debug, Clone)]
pub struct ReadTool {
    skill_resolver: Option<SkillResolver>,
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadTool {
    /// 创建不支持 `skill://` 的基础 read 工具。
    pub const fn new() -> Self {
        Self {
            skill_resolver: None,
        }
    }

    /// 创建支持 `skill://` 的 read 工具。
    pub const fn with_skill_resolver(skill_resolver: SkillResolver) -> Self {
        Self {
            skill_resolver: Some(skill_resolver),
        }
    }

    async fn execute_read(&self, params: ReadParams) -> Result<ToolResult, ToolError> {
        tracing::debug!(path = %params.path, offset = ?params.offset, limit = ?params.limit, "read");
        if let Some(target) = params.path.strip_prefix(SKILL_SCHEME) {
            return self.execute_skill_read(&params, target).await;
        }

        read_text_path(
            Path::new(&params.path),
            &params.path,
            None,
            params.offset,
            params.limit,
        )
        .await
    }

    /// `skill://<name>[/<path>]` 读取：无子路径返回 SKILL.md 正文；
    /// 子路径返回 skill 根目录内的文件内容或目录清单。
    async fn execute_skill_read(
        &self,
        params: &ReadParams,
        target: &str,
    ) -> Result<ToolResult, ToolError> {
        let resolver = self.skill_resolver.as_ref().ok_or_else(|| {
            ToolError::new(format!(
                "Skill reading is not configured for this read tool. \
                 Use a filesystem path, or start nomic from a directory where skills can be discovered. \
                 Requested: {SKILL_SCHEME}{target}"
            ))
        })?;
        // 首个 `/` 切分 skill 名与子路径；`skill://name/` 的子路径为空串，
        // 与无子路径同义（正文）。
        let (name, rel) = match target.split_once('/') {
            Some((name, rel)) => (name, Some(rel)),
            None => (target, None),
        };
        let resolve_error = |error: SkillsError| {
            ToolError::new(format!("Could not resolve {SKILL_SCHEME}{target}. {error}"))
        };
        let resource = resolver
            .resolve_resource(name, rel)
            .map_err(resolve_error)?;
        match resource {
            SkillResource::Instructions(skill) => {
                let mut result = read_text_path(
                    &skill.path,
                    &params.path,
                    Some(skill.document.body.clone()),
                    params.offset,
                    params.limit,
                )
                .await?;
                result.details = Some(merge_details(
                    result.details.take(),
                    &serde_json::json!({
                        "source": skill_source(&params.path, &skill, None),
                    }),
                ));
                Ok(result)
            }
            SkillResource::File { skill, path } => {
                let rel_display = rel.unwrap_or_default().to_string();
                let mut result =
                    read_text_path(&path, &params.path, None, params.offset, params.limit).await?;
                result.details = Some(merge_details(
                    result.details.take(),
                    &serde_json::json!({
                        "source": skill_source(&params.path, &skill, Some(rel_display.as_str())),
                    }),
                ));
                Ok(result)
            }
            SkillResource::Directory { skill, path } => {
                let listing = read_dir_listing(&path).await.map_err(|error| {
                    ToolError::new(format!(
                        "Could not read directory: {}. {error}",
                        params.path
                    ))
                })?;
                let mut result = ToolResult::text(listing);
                let rel_display = format!("{}/", rel.unwrap_or_default().trim_end_matches('/'));
                result.details = Some(serde_json::json!({
                    "source": skill_source(&params.path, &skill, Some(rel_display.as_str())),
                }));
                Ok(result)
            }
        }
    }
}

const LABEL: &str = "read";

const DESCRIPTION: &str = "Read the contents of a file or skill://<name>[/<path>]. Supports text files and read-only skill instructions; a sub-path reads a file inside the skill directory, and a directory sub-path lists its entries. Output is truncated to 2000 lines or 50KB \
         (whichever is hit first). Use offset/limit for large files. When you need the full file, \
         continue with offset until complete.";

#[async_trait]
impl AgentTool for ReadTool {
    type Params = ReadParams;

    fn name(&self) -> &'static str {
        "read"
    }

    fn label(&self) -> &str {
        LABEL
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    async fn execute(
        &self,
        params: Self::Params,
        _cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        self.execute_read(params).await
    }
}

/// 按现有文本契约读取本地 UTF-8 文件。
async fn read_text_path(
    file_path: &Path,
    display_path: &str,
    content_override: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<ToolResult, ToolError> {
    let content = if let Some(content) = content_override {
        content
    } else {
        tokio::fs::read_to_string(file_path)
            .await
            .map_err(|e| ToolError::new(format!("Could not read file: {display_path}. {e}")))?
    };

    let lines: Vec<&str> = content.split('\n').collect();
    let total_file_lines = lines.len();
    let start_line = offset.map_or(0, |o| o.saturating_sub(1));
    let start_line_display = start_line + 1;
    if start_line >= lines.len() {
        return Err(ToolError::new(format!(
            "Offset {} is beyond end of file ({total_file_lines} lines total)",
            offset.unwrap_or(1)
        )));
    }

    let (selected, user_limited_lines) = if let Some(limit) = limit {
        let end = (start_line + limit).min(lines.len());
        (lines[start_line..end].join("\n"), Some(end - start_line))
    } else {
        (lines[start_line..].join("\n"), None)
    };

    let truncation = truncate_head(&selected, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
    let output_text = if truncation.first_line_exceeds_limit {
        exceeds_notice(
            start_line_display,
            lines[start_line].len(),
            DEFAULT_MAX_BYTES,
            &format!(
                "Use bash: sed -n '{start_line_display}p' {} | head -c {DEFAULT_MAX_BYTES}",
                file_path.display()
            ),
        )
    } else if let Some(notice) =
        truncation.notice(start_line_display, total_file_lines, &Continuation::Offset)
    {
        format!("{}\n\n{notice}", truncation.content)
    } else if let Some(limited) = user_limited_lines
        && start_line + limited < lines.len()
    {
        let remaining = lines.len() - (start_line + limited);
        let next_offset = start_line + limited + 1;
        format!(
            "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
            truncation.content
        )
    } else {
        truncation.content
    };

    let mut result = ToolResult::text(output_text);
    if truncation.truncated {
        result.details = Some(serde_json::json!({
            "truncation": {
                "truncated_by": truncation.truncated_by.map(|b| match b {
                    TruncatedBy::Lines => "lines",
                    TruncatedBy::Bytes => "bytes",
                }),
                "total_lines": truncation.total_lines,
                "output_lines": truncation.output_lines,
            }
        }));
    }
    Ok(result)
}

/// 合并 details 的顶层字段，保留已有 truncation 信息。
fn merge_details(base: Option<serde_json::Value>, extra: &serde_json::Value) -> serde_json::Value {
    let mut merged = base
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            merged.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(merged)
}

/// `details.source` 的 skill 标注；`resource` 为子路径（目录以 `/` 结尾），无子路径时为 `None`。
fn skill_source(
    uri: &str,
    skill: &nomic_skills::Skill,
    resource: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "skill",
        "uri": uri,
        "name": skill.name,
        "scope": skill.scope.to_string(),
        "path": skill.path.display().to_string(),
        "resource": resource,
    })
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
