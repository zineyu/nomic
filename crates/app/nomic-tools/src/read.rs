//! `read` 工具：文件 / 内部 URI（`skill://` 等）读取、offset/limit、
//! 尾挂选择器（`:N-M` / `:raw`）、头部截断与翻页提示。
//!
//! 内部 URI 走 [`VfsRouter`] 挂载分发（ADR-0040 / ADR-0042）；普通路径走文件系统。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use nomic_skills::{SKILL_SCHEME, SkillResolver};
use nomic_vfs::fs::SkillVfs;
use nomic_vfs::parse::{parse_internal_uri, percent_decode};
use nomic_vfs::{
    LineRange, ParsedSelector, VfsFile, VfsRouter, parse_selector, split_uri_selector,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::truncate::{
    Continuation, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, exceeds_notice, truncate_head,
};

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Path to the file to read (relative or absolute), or an internal URI
    /// like skill://<name>[/<path>] (optionally with a trailing line selector
    /// such as :50-100 or :raw)
    pub path: String,
    /// Line number to start reading from (1-indexed)
    pub offset: Option<usize>,
    /// Maximum number of lines to read
    pub limit: Option<usize>,
}

/// `read` 工具。
#[derive(Debug, Clone)]
pub struct ReadTool {
    /// VFS 挂载表；`None` 时仅支持文件系统路径
    vfs_router: Option<Arc<VfsRouter>>,
    /// 相对路径的解析基准（workspace 严格归属；空句柄 = 进程 cwd）
    base: crate::base::BaseDir,
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadTool {
    /// 创建不支持内部 URI 的基础 read 工具。
    pub fn new() -> Self {
        Self {
            vfs_router: None,
            base: crate::base::BaseDir::default(),
        }
    }

    /// 创建支持 `skill://` 的 read 工具（以 skill resolver 构造单协议挂载）。
    pub fn with_skill_resolver(skill_resolver: SkillResolver) -> Self {
        let mut router = VfsRouter::new();
        router.mount(Arc::new(SkillVfs::new(skill_resolver)));
        Self::with_vfs_router(Arc::new(router))
    }

    /// 创建带 VFS 挂载表的 read 工具。
    pub fn with_vfs_router(vfs_router: Arc<VfsRouter>) -> Self {
        Self {
            vfs_router: Some(vfs_router),
            base: crate::base::BaseDir::default(),
        }
    }

    /// 设置固定基准目录：相对路径以它解析（workspace 严格归属）。
    #[must_use]
    pub fn with_base_dir(mut self, base_dir: Option<PathBuf>) -> Self {
        self.base = crate::base::BaseDir::new(base_dir);
        self
    }

    /// 共享基准目录句柄：句柄更新后本工具的下一次执行即用新基准
    ///（交互端切换 session 的 workspace 场景）。
    #[must_use]
    pub fn with_shared_base_dir(mut self, base: &crate::base::BaseDir) -> Self {
        self.base = base.clone();
        self
    }

    async fn execute_read(&self, params: ReadParams) -> Result<ToolResult, ToolError> {
        tracing::debug!(path = %params.path, offset = ?params.offset, limit = ?params.limit, "read");
        let target = params.path.trim();
        if target.is_empty() {
            return Err(ToolError::new("path is empty"));
        }
        if let Some(router) = &self.vfs_router {
            // 已挂载 scheme → 路由；形似 URI 但未挂载 → 也交给 router 报
            // UnknownScheme（带可用 scheme 列表），比文件 ENOENT 更可行动。
            if router.can_resolve(target) || VfsRouter::looks_like_uri(target) {
                return self.execute_uri_read(router, target, &params).await;
            }
        } else if let Some(uri_target) = target.strip_prefix(SKILL_SCHEME) {
            return Err(ToolError::new(format!(
                "Skill reading is not configured for this read tool. \
                 Use a filesystem path, or start nomic from a directory where skills can be discovered. \
                 Requested: {SKILL_SCHEME}{uri_target}"
            )));
        }

        let base = self.base.snapshot();
        read_text_path(
            &crate::base::resolve(base.as_deref(), target),
            target,
            None,
            params.offset,
            params.limit,
        )
        .await
    }
}

/// 读取 `:conflicts` 的一侧文件（相对 read 工具基准目录）。
async fn read_conflict_side(base: Option<&Path>, param: &str) -> Result<String, ToolError> {
    let path = crate::base::resolve(base, &percent_decode(param));
    tokio::fs::read_to_string(&path).await.map_err(|error| {
        ToolError::new(format!(
            "Could not read {param}: {}. {error}",
            path.display()
        ))
    })
}

/// `:conflicts` 的 query 参数名。
const CONFLICTS_BASE_PARAM: &str = "base";
/// `:conflicts` 的 query 参数名。
const CONFLICTS_THEIRS_PARAM: &str = "theirs";

impl ReadTool {
    /// 内部 URI 读取：剥选择器 → 挂载分发 → 选择器/分页 → 合并 details。
    async fn execute_uri_read(
        &self,
        router: &VfsRouter,
        target: &str,
        params: &ReadParams,
    ) -> Result<ToolResult, ToolError> {
        let (clean, sel) = split_uri_selector(target);
        let selector = match sel {
            Some(sel) => parse_selector(&sel).map_err(|error| ToolError::new(error.to_string()))?,
            None => ParsedSelector::None,
        };
        let file = router
            .read(&clean)
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        if matches!(selector, ParsedSelector::Conflicts) {
            return self.read_conflicts(&file, &clean).await;
        }
        read_resource(&file, &selector, params).await
    }

    /// `:conflicts` 选择器：以资源内容为 ours，`?theirs=`（必填）与
    /// `?base=`（可选）为文件系统路径，输出冲突行区间的切片。
    async fn read_conflicts(&self, file: &VfsFile, clean: &str) -> Result<ToolResult, ToolError> {
        let url = parse_internal_uri(clean).map_err(|error| ToolError::new(error.to_string()))?;
        let Some(theirs_param) = url.query_param(CONFLICTS_THEIRS_PARAM) else {
            return Err(ToolError::new(format!(
                "The :conflicts selector needs a comparison file: \
                 {clean}?{CONFLICTS_THEIRS_PARAM}=<path> \
                 (optionally &{CONFLICTS_BASE_PARAM}=<path>)."
            )));
        };
        let base_dir = self.base.snapshot();
        let theirs = read_conflict_side(base_dir.as_deref(), theirs_param).await?;
        let base = match url.query_param(CONFLICTS_BASE_PARAM) {
            Some(param) => Some(read_conflict_side(base_dir.as_deref(), param).await?),
            None => None,
        };
        let ranges = crate::conflicts::find_conflicts(&file.content, base.as_deref(), &theirs)
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        let conflict_details = serde_json::json!({
            "conflicts": ranges
                .iter()
                .map(|range| serde_json::json!([range.start, range.end, range.empty]))
                .collect::<Vec<_>>(),
        });
        if ranges.is_empty() {
            let mut result =
                ToolResult::text(format!("No conflicts found in {}.", url.without_query()));
            result.details = Some(merge_details(file.details.clone(), &conflict_details));
            return Ok(result);
        }
        let line_ranges: Vec<LineRange> = ranges
            .iter()
            .map(|range| LineRange {
                start: range.start,
                end: Some(range.end),
            })
            .collect();
        let joined = slice_line_ranges(&file.content, &line_ranges, &file.url)?;
        let hint = file.meta.source_path.clone();
        let mut result = read_text_path(&hint, &file.url, Some(joined), None, None).await?;
        result.details = Some(merge_details(
            Some(merge_details(result.details.take(), &conflict_details)),
            file.details.as_ref().unwrap_or(&serde_json::Value::Null),
        ));
        Ok(result)
    }
}

/// 对已读取的资源应用选择器与分页。显式 offset/limit 参数优先于尾挂选择器。
async fn read_resource(
    file: &VfsFile,
    selector: &ParsedSelector,
    params: &ReadParams,
) -> Result<ToolResult, ToolError> {
    let hint = file.meta.source_path.clone();
    let display = file.url.as_str();
    let explicit = params.offset.is_some() || params.limit.is_some();
    let mut result = if explicit {
        read_text_path(
            &hint,
            display,
            Some(file.content.clone()),
            params.offset,
            params.limit,
        )
        .await?
    } else {
        match selector {
            ParsedSelector::Lines { ranges, .. } if ranges.len() > 1 => {
                let joined = slice_line_ranges(&file.content, ranges, display)?;
                read_text_path(&hint, display, Some(joined), None, None).await?
            }
            ParsedSelector::Lines { .. } => {
                let (offset, limit) = selector.to_offset_limit().unwrap_or((None, None));
                read_text_path(&hint, display, Some(file.content.clone()), offset, limit).await?
            }
            // Raw / None：nomic 的 read 本无结构加工，raw 等价于完整读取
            _ => read_text_path(&hint, display, Some(file.content.clone()), None, None).await?,
        }
    };
    if let Some(details) = &file.details {
        result.details = Some(merge_details(result.details.take(), details));
    }
    Ok(result)
}

/// 多段行范围切片：各段内容以 `[...]` 分隔拼接（越界段报错）。
fn slice_line_ranges(
    content: &str,
    ranges: &[LineRange],
    display: &str,
) -> Result<String, ToolError> {
    let lines: Vec<&str> = content.split('\n').collect();
    let total = lines.len();
    let mut sections = Vec::with_capacity(ranges.len());
    for range in ranges {
        if range.start > total {
            return Err(ToolError::new(format!(
                "Line range :{} is beyond end of {display} ({total} lines total)",
                range.start
            )));
        }
        let start = range.start - 1;
        let end = range.end.map_or(total, |end| end.min(total));
        sections.push(lines[start..end].join("\n"));
    }
    Ok(sections.join("\n\n[...]\n\n"))
}

const LABEL: &str = "read";

const DESCRIPTION: &str = "Read the contents of a file or an internal URI like skill://<name>[/<path>]. Supports text files and read-only skill instructions; a sub-path reads a file inside the skill directory, and a directory sub-path lists its entries. Output is truncated to 2000 lines or 50KB \
         (whichever is hit first). Use offset/limit for large files. When you need the full file, \
         continue with offset until complete. Internal URIs accept trailing selectors: \
         :N-M (line range), :raw, :conflicts (with ?theirs=<path>[, ?base=<path>]).";

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
