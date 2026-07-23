//! `read` 工具：offset/limit + 头部截断 + 翻页引导提示（契约与 pi 一致）。

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, format_size, truncate_head,
};

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Path to the file to read (relative or absolute)
    pub path: String,
    /// Line number to start reading from (1-indexed)
    pub offset: Option<usize>,
    /// Maximum number of lines to read
    pub limit: Option<usize>,
}

/// `read` 工具。
#[derive(Debug, Default, Clone, Copy)]
pub struct ReadTool;

const LABEL: &str = "read";

const DESCRIPTION: &str = "Read the contents of a file. Supports text files. Output is truncated to 2000 lines or 50KB \
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
        let content = tokio::fs::read_to_string(&params.path)
            .await
            .map_err(|e| ToolError::new(format!("Could not read file: {}. {e}", params.path)))?;

        let lines: Vec<&str> = content.split('\n').collect();
        let total_file_lines = lines.len();
        let start_line = params.offset.map_or(0, |o| o.saturating_sub(1));
        let start_line_display = start_line + 1;
        if start_line >= lines.len() {
            return Err(ToolError::new(format!(
                "Offset {} is beyond end of file ({total_file_lines} lines total)",
                params.offset.unwrap_or(1)
            )));
        }

        let (selected, user_limited_lines) = if let Some(limit) = params.limit {
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
                params.path
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
}
