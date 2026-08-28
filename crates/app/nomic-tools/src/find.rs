//! `find` 工具：基于 fff 常驻索引的文件 / 目录查找（glob 匹配、gitignore 感知）。
//!
//! 文件枚举走 fff 的常驻索引（后台扫描 + watcher 保持新鲜），不再逐次遍历
//! 文件系统；glob 过滤与排序在输出侧完成，语义与旧的 fd 风格实现一致。
//!
//! fff 语义差异：git 仓库内索引隐藏文件（`.git`/`.jj` 输出侧过滤）；
//! 非 git 目录跳过隐藏文件，且 .gitignore 不生效（fff 以硬编码的重型
//! 目录清单替代）。

use std::path::PathBuf;

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
#[derive(Debug, Default, Clone)]
pub struct FindTool {
    /// 相对路径的解析基准（workspace 严格归属；空句柄 = 进程 cwd）
    base: crate::base::BaseDir,
}

impl FindTool {
    /// 创建以进程 cwd 为基准的 find 工具。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置固定基准目录：相对搜索根以它解析（workspace 严格归属）。
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
}

const LABEL: &str = "find";

const DESCRIPTION: &str = "Find files and directories by glob pattern, fd-style. A pattern without \"/\" matches \
         file names at any depth (\"*.toml\"); a pattern with \"/\" matches paths relative to the \
         search root (\"crates/*/src\"). Backed by the fff index (watched, always fresh). Respects \
         .gitignore inside git repositories; hidden files are included inside git repositories \
         only. Returns paths sorted alphabetically, capped at 200 results (raise with `limit`). \
         Prefer this over running find/fd/ls via bash.";

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
        let root =
            crate::base::resolve_root(self.base.snapshot().as_deref(), params.path.as_deref());
        if !root.is_dir() {
            return Err(ToolError::new(format!(
                "Path is not a directory: {}",
                root.display()
            )));
        }
        let kind = params.kind;
        tracing::debug!(pattern = %params.pattern, root = %root.display(), limit, "find");

        let picker = crate::picker::picker_for(&root)?;
        let find_root = root.clone();
        let found =
            tokio::task::spawn_blocking(move || find(&picker, &find_root, &matcher, kind, limit))
                .await
                .map_err(|e| ToolError::new(format!("Find task failed: {e}")))??;

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

/// 在 fff 索引上匹配 glob（文件 + 目录），按字典序排序后截断到 limit。
fn find(
    picker: &fff_search::SharedFilePicker,
    root: &std::path::Path,
    matcher: &globset::GlobMatcher,
    kind: Option<FindKind>,
    limit: usize,
) -> Result<Found, ToolError> {
    let guard = picker
        .read()
        .map_err(|e| ToolError::new(format!("Index unavailable: {e}")))?;
    let picker = guard
        .as_ref()
        .ok_or_else(|| ToolError::new("Index not initialized"))?;

    let want_files = !matches!(kind, Some(FindKind::Dir));
    let want_dirs = !matches!(kind, Some(FindKind::File));
    let mut paths = Vec::new();
    if want_files {
        paths.extend(
            picker
                .get_files()
                .iter()
                .filter(|f| !f.is_deleted())
                .map(|f| f.relative_path(picker))
                .filter(|rel| !crate::picker::is_vcs_internal_path(rel))
                .filter(|rel| matcher.is_match(rel.as_str()))
                .map(|rel| display_path(root, &rel)),
        );
    }
    if want_dirs {
        paths.extend(
            picker
                .get_dirs()
                .iter()
                .filter(|d| !d.is_deleted())
                .map(|d| d.relative_path(picker))
                // 索引内目录路径以 `/` 结尾，匹配与展示前去掉
                .map(|rel| rel.trim_end_matches('/').to_string())
                .filter(|rel| !rel.is_empty() && !crate::picker::is_vcs_internal_path(rel))
                .filter(|rel| matcher.is_match(rel.as_str()))
                .map(|rel| display_path(root, &rel)),
        );
    }
    paths.sort();
    drop(guard);
    let truncated = paths.len() > limit;
    paths.truncate(limit);
    Ok(Found { paths, truncated })
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

/// 展示路径：搜索根为 `.` 时拼接结果带 `./` 前缀，去掉以保持简洁。
fn display_path(root: &std::path::Path, relative: &str) -> String {
    let text = root.join(relative).display().to_string();
    match text.strip_prefix("./") {
        Some(stripped) => stripped.to_string(),
        None => text,
    }
}
