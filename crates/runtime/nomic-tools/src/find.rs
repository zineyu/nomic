//! `find` 工具：fd 语义的文件 / 目录查找（glob 匹配、gitignore 感知）。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// 默认最多返回的结果数。
const DEFAULT_LIMIT: usize = 200;

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindParams {
    /// Glob pattern matched against file names ("*.rs") or paths relative
    /// to the search root when it contains "/" ("src/**/*.rs")
    pub pattern: String,
    /// Directory to search (default: current directory)
    pub path: Option<String>,
    /// Restrict result kind: "file" or "dir" (default: both)
    pub kind: Option<FindKind>,
    /// Maximum number of results to return (default 200)
    pub limit: Option<usize>,
    /// Also include hidden files and directories
    pub include_hidden: Option<bool>,
}

/// 结果类型过滤。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FindKind {
    /// 仅文件
    File,
    /// 仅目录
    Dir,
}

/// `find` 工具。
#[derive(Debug, Default, Clone, Copy)]
pub struct FindTool;

const LABEL: &str = "find";

const DESCRIPTION: &str = "Find files and directories by glob pattern, fd-style. A pattern without \"/\" matches \
         file names at any depth (\"*.toml\"); a pattern with \"/\" matches paths relative to the \
         search root (\"crates/*/src\"). Respects .gitignore and skips hidden files unless \
         include_hidden is set. Returns paths sorted alphabetically, capped at 200 results \
         (raise with `limit`). Prefer this over running find/fd/ls via bash.";

#[async_trait]
impl AgentTool for FindTool {
    type Params = FindParams;

    fn name(&self) -> &'static str {
        "find"
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
        cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::new("Find aborted"));
        }
        if params.pattern.is_empty() {
            return Err(ToolError::new("Pattern must not be empty"));
        }
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 {
            return Err(ToolError::new("Limit must be at least 1"));
        }
        let matcher = build_matcher(&params.pattern)?;
        let root = PathBuf::from(params.path.as_deref().unwrap_or("."));
        if !root.is_dir() {
            return Err(ToolError::new(format!(
                "Path is not a directory: {}",
                root.display()
            )));
        }
        let include_hidden = params.include_hidden.unwrap_or(false);
        let kind = params.kind;
        tracing::debug!(pattern = %params.pattern, root = %root.display(), limit, "find");

        let find_root = root.clone();
        let found = tokio::task::spawn_blocking(move || {
            find(&find_root, &matcher, kind, include_hidden, limit)
        })
        .await
        .map_err(|e| ToolError::new(format!("Find task failed: {e}")))?;

        if found.paths.is_empty() {
            return Ok(ToolResult::text(format!(
                "No files found matching {:?} in {}",
                params.pattern,
                root.display()
            )));
        }
        let mut output = found.paths.join("\n");
        if found.truncated {
            use std::fmt::Write as _;
            let _ = write!(
                output,
                "\n[Limit of {limit} results reached; more results exist. \
                 Use a more specific pattern/path or raise limit to see more.]"
            );
        }
        let mut result = ToolResult::text(output);
        result.details = Some(serde_json::json!({
            "result_count": found.paths.len(),
            "truncated": found.truncated,
        }));
        Ok(result)
    }
}

/// 查找结果。
struct Found {
    paths: Vec<String>,
    truncated: bool,
}

/// 遍历 root 并匹配 glob，结果按字典序排序后截断到 limit。
fn find(
    root: &Path,
    matcher: &globset::GlobMatcher,
    kind: Option<FindKind>,
    include_hidden: bool,
    limit: usize,
) -> Found {
    let mut paths: Vec<String> = crate::walk::walk(root, include_hidden)
        .into_iter()
        .filter(|entry| match (kind, entry.file_type()) {
            (Some(FindKind::File), Some(t)) => t.is_file(),
            (Some(FindKind::Dir), Some(t)) => t.is_dir(),
            _ => true,
        })
        .filter(|entry| {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap_or_else(|_| entry.path());
            matcher.is_match(relative)
        })
        .map(|entry| {
            let text = entry.path().display().to_string();
            match text.strip_prefix("./") {
                Some(stripped) => stripped.to_string(),
                None => text,
            }
        })
        .collect();
    paths.sort();
    let truncated = paths.len() > limit;
    paths.truncate(limit);
    Found { paths, truncated }
}

/// 构造 glob 匹配器：纯文件名模式（不含 `/`）等价于任意深度匹配。
fn build_matcher(pattern: &str) -> Result<globset::GlobMatcher, ToolError> {
    let pattern = if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    };
    let glob = globset::GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| ToolError::new(format!("Invalid glob {pattern:?}: {e}")))?;
    Ok(glob.compile_matcher())
}
