//! `read` 工具：文件 / `skill://` 读取、offset/limit、头部截断与翻页提示。

use std::path::Path;

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use nomic_skills::SkillResolver;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, format_size, truncate_head,
};

const SKILL_SCHEME: &str = "skill://";

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Path to the file to read (relative or absolute), or skill://<name>
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
        if let Some(name) = params.path.strip_prefix(SKILL_SCHEME) {
            let resolver = self.skill_resolver.as_ref().ok_or_else(|| {
                ToolError::new(format!(
                    "Skill reading is not configured for this read tool. \
                     Use a filesystem path, or start nomic from a directory where skills can be discovered. \
                     Requested: {SKILL_SCHEME}{name}"
                ))
            })?;
            let skill = resolver.resolve(name).map_err(|error| {
                ToolError::new(format!("Could not resolve {SKILL_SCHEME}{name}. {error}"))
            })?;
            let mut result = read_text_path(
                &skill.path,
                &params.path,
                Some(skill.document.body),
                params.offset,
                params.limit,
            )
            .await?;
            result.details = Some(merge_details(
                result.details.take(),
                &serde_json::json!({
                    "source": {
                        "kind": "skill",
                        "uri": params.path,
                        "name": skill.name,
                        "scope": skill.scope.to_string(),
                        "path": skill.path.display().to_string(),
                    }
                }),
            ));
            return Ok(result);
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
}

const LABEL: &str = "read";

const DESCRIPTION: &str = "Read the contents of a file or skill://<name>. Supports text files and read-only skill instructions. Output is truncated to 2000 lines or 50KB \
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
        let first_line_size = format_size(lines[start_line].len());
        format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. \
             Use bash: sed -n '{start_line_display}p' {} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(DEFAULT_MAX_BYTES),
            file_path.display()
        )
    } else if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines - 1;
        let next_offset = end_line_display + 1;
        let suffix = if truncation.truncated_by == Some(TruncatedBy::Lines) {
            format!(
                "[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. \
                 Use offset={next_offset} to continue.]"
            )
        } else {
            format!(
                "[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} \
                 ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            )
        };
        format!("{}\n\n{suffix}", truncation.content)
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
