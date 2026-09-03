//! `grep` 工具：基于 fff 常驻索引的内容搜索（正则 / 字面量、gitignore 感知、
//! glob 过滤）。
//!
//! 与旧的逐文件遍历实现不同，搜索命中 fff 的常驻索引（后台扫描 + watcher
//! 保持新鲜）：文件枚举与二进制/体积过滤由索引完成，行匹配由 fff 的
//! SIMD memmem（字面量）/ regex（正则）引擎执行。
//!
//! fff 语义与旧实现的差异：
//! - git 仓库内索引隐藏文件（`.git`/`.jj` 输出侧过滤）；非 git 目录跳过
//!   隐藏文件，且 .gitignore 不生效（fff 以硬编码的重型目录清单替代）；
//! - 大于 10MB 的文件不搜索（fff `MAX_FFFILE_SIZE`）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use fff_search::{Constraint, FFFQuery, FuzzyQuery, GrepMode, GrepSearchOptions};
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use nomic_vfs::VfsRouter;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// 默认最多返回的匹配数。
const DEFAULT_LIMIT: usize = 100;
/// 单条匹配行的最大字符数（应对压缩过的长行）。
const MAX_LINE_CHARS: usize = 500;

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepParams {
    /// Regex pattern to search for (set `literal` to search a fixed string)
    pub pattern: String,
    /// File or directory to search (default: current directory)
    pub path: Option<String>,
    /// Glob restricting which files are searched, e.g. "*.rs" or "src/**/*.ts"
    pub glob: Option<String>,
    /// Case-insensitive search
    pub ignore_case: Option<bool>,
    /// Treat `pattern` as a literal string instead of a regex
    pub literal: Option<bool>,
    /// Maximum number of matches to return (default 100)
    pub limit: Option<usize>,
}

/// `grep` 工具。
#[derive(Debug, Default, Clone)]
pub struct GrepTool {
    /// 内部 URI 挂载表：搜索根可为 `skill://` / `local://` 等 URI
    ///（对齐到底层 source_path）；`None` 时仅支持文件系统路径
    vfs_router: Option<Arc<VfsRouter>>,
    /// 相对路径的解析基准（project 严格归属；空句柄 = 进程 cwd）
    base: crate::base::BaseDir,
}

impl GrepTool {
    /// 创建以进程 cwd 为基准的 grep 工具。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置固定基准目录：相对搜索根以它解析（project 严格归属）。
    #[must_use]
    pub fn with_base_dir(mut self, base_dir: Option<PathBuf>) -> Self {
        self.base = crate::base::BaseDir::new(base_dir);
        self
    }

    /// 共享基准目录句柄：句柄更新后本工具的下一次执行即用新基准
    ///（交互端切换 session 的 project 场景）。
    #[must_use]
    pub fn with_shared_base_dir(mut self, base: &crate::base::BaseDir) -> Self {
        self.base = base.clone();
        self
    }

    /// 挂 VFS 挂载表（会话共享实例）。
    #[must_use]
    pub fn with_vfs_router(mut self, vfs_router: Arc<VfsRouter>) -> Self {
        self.vfs_router = Some(vfs_router);
        self
    }
}

const LABEL: &str = "grep";

const DESCRIPTION: &str = "Search file contents with a regex pattern, ripgrep-style. Backed by the fff index \
         (watched, always fresh), so repeated searches are near-instant. Respects .gitignore inside \
         git repositories and skips binary files; hidden files are searched inside git repositories \
         only. Returns matching lines as \"path:line: content\" sorted by file and line number, \
         capped at 100 matches (raise with `limit`). Use `literal` for fixed-string search, `glob` \
         to restrict file types, and `ignore_case` for case-insensitive matching. Prefer this over \
         running grep/rg via bash.";

#[async_trait]
impl AgentTool for GrepTool {
    type Params = GrepParams;

    fn name(&self) -> &'static str {
        "grep"
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
            return Err(ToolError::new("Search aborted"));
        }
        if params.pattern.is_empty() {
            return Err(ToolError::new("Pattern must not be empty"));
        }
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 {
            return Err(ToolError::new("Limit must be at least 1"));
        }
        let ignore_case = params.ignore_case.unwrap_or(false);
        let mode = if params.literal.unwrap_or(false) {
            GrepMode::PlainText
        } else {
            GrepMode::Regex
        };
        // 大小写映射：fff 只有 smart_case（全小写才不敏感），与工具的显式
        // ignore_case 语义不同。正则模式用 `(?i)` 前缀强制不敏感；字面量模式
        // 将 pattern 转小写后借 smart_case 得到等价的不敏感匹配。
        let effective = match (mode, ignore_case) {
            (GrepMode::Regex, true) => format!("(?i){}", params.pattern),
            (GrepMode::PlainText, true) => params.pattern.to_lowercase(),
            _ => params.pattern.clone(),
        };
        // 预先以同一 regex 引擎校验，保持 "Invalid regex" 错误契约
        //（fff 对非法正则是回退字面量而非报错）。
        if mode == GrepMode::Regex {
            regex::Regex::new(&effective)
                .map_err(|e| ToolError::new(format!("Invalid regex: {e}")))?;
        }
        let glob = params.glob.as_deref().map(normalize_glob).transpose()?;
        // 内部 URI 搜索根：对齐到底层 source_path（ADR-0040 §9）
        let uri_root = match (&self.vfs_router, params.path.as_deref()) {
            (Some(router), Some(path)) => {
                crate::vfs_guard::vfs_source_path(router, path, "grep").await?
            }
            _ => None,
        };
        let root = match uri_root {
            Some(root) => root,
            None => {
                crate::base::resolve_root(self.base.snapshot().as_deref(), params.path.as_deref())
            }
        };
        tracing::debug!(pattern = %params.pattern, root = %root.display(), limit, "grep");

        // 单文件根不进索引：直接读内容匹配（索引父目录的代价不可控）。
        if root.is_file() {
            let outcome = tokio::task::spawn_blocking({
                let root = root.clone();
                let effective = effective.clone();
                move || search_single_file(&root, &effective, mode, ignore_case, limit)
            })
            .await
            .map_err(|e| ToolError::new(format!("Search task failed: {e}")))??;
            return Ok(render(&params.pattern, &root, &outcome, limit));
        }
        if !root.is_dir() {
            return Err(ToolError::new(format!(
                "Path does not exist: {}",
                root.display()
            )));
        }

        let picker = crate::picker::picker_for(&root)?;
        // 取消令牌 → fff 中止信号（跨线程桥的轻量轮询由 fff 内部完成）。
        let abort = Arc::new(AtomicBool::new(false));
        let abort_watcher = {
            let abort = Arc::clone(&abort);
            let cancel = cancel.clone();
            tokio::spawn(async move {
                cancel.cancelled().await;
                abort.store(true, Ordering::Release);
            })
        };

        let search_root = root.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            search_indexed(
                &picker,
                &search_root,
                &effective,
                glob.as_deref(),
                mode,
                ignore_case,
                limit,
                &abort,
            )
        })
        .await
        .map_err(|e| ToolError::new(format!("Search task failed: {e}")))?;
        abort_watcher.abort();
        let outcome = outcome?;

        Ok(render(&params.pattern, &root, &outcome, limit))
    }
}

/// 一条匹配。
struct Match {
    path: String,
    line: usize,
    content: String,
}

/// 搜索结果。
struct Outcome {
    matches: Vec<Match>,
    truncated: bool,
}

/// 在 fff 索引上执行内容搜索。
#[allow(clippy::too_many_arguments)]
fn search_indexed(
    picker: &fff_search::SharedFilePicker,
    root: &Path,
    effective: &str,
    glob: Option<&str>,
    mode: GrepMode,
    ignore_case: bool,
    limit: usize,
    abort: &Arc<AtomicBool>,
) -> Result<Outcome, ToolError> {
    let guard = picker
        .read()
        .map_err(|e| ToolError::new(format!("Index unavailable: {e}")))?;
    let picker = guard
        .as_ref()
        .ok_or_else(|| ToolError::new("Index not initialized"))?;

    let constraints: Vec<Constraint<'_>> = glob.into_iter().map(Constraint::Glob).collect();
    let query = FFFQuery {
        raw_query: effective,
        constraints,
        fuzzy_query: FuzzyQuery::Text(effective),
        location: None,
    };
    let options = GrepSearchOptions {
        mode,
        // 字面量 + ignore_case：配合转小写的 pattern 得到不敏感匹配；
        // 其余情况保持大小写敏感（smart_case 全小写不敏感不符合工具契约）。
        smart_case: ignore_case && mode == GrepMode::PlainText,
        page_limit: limit,
        max_matches_per_file: limit,
        abort_signal: Some(Arc::clone(abort)),
        ..Default::default()
    };
    let result = picker.grep(&query, &options);
    if let Some(err) = &result.regex_fallback_error {
        return Err(ToolError::new(format!("Invalid regex: {err}")));
    }

    let mut matches = Vec::with_capacity(result.matches.len().min(limit));
    for m in &result.matches {
        let Some(file) = result.files.get(m.file_index) else {
            continue;
        };
        let relative = file.relative_path(picker);
        if crate::picker::is_vcs_internal_path(&relative) {
            continue;
        }
        matches.push(Match {
            path: display_path(&root.join(&relative)),
            line: usize::try_from(m.line_number).unwrap_or(usize::MAX),
            content: truncate_line(m.line_content.trim_end_matches(['\n', '\r'])),
        });
    }
    let more_pages = result.next_file_offset != 0;
    drop(guard);
    Ok(finish(matches, limit, more_pages))
}

/// 单文件内容搜索（root 为文件时的特判路径，不进索引）。
fn search_single_file(
    path: &Path,
    effective: &str,
    mode: GrepMode,
    ignore_case: bool,
    limit: usize,
) -> Result<Outcome, ToolError> {
    let line_regex = match mode {
        GrepMode::Regex => regex::Regex::new(effective),
        // fff 还有 Fuzzy 模式，本工具不暴露。
        GrepMode::PlainText | GrepMode::Fuzzy => {
            regex::RegexBuilder::new(&regex::escape(effective))
                .case_insensitive(ignore_case)
                .build()
        }
    }
    .map_err(|e| ToolError::new(format!("Invalid regex: {e}")))?;
    let content = std::fs::read(path)
        .map_err(|e| ToolError::new(format!("Cannot read {}: {e}", path.display())))?;
    // 与旧实现一致的二进制契约：含 NUL 字节的文件整体跳过。
    if content.contains(&b'\x00') {
        return Ok(Outcome {
            matches: Vec::new(),
            truncated: false,
        });
    }
    let display = display_path(path);
    let text = String::from_utf8_lossy(&content);
    let mut matches = Vec::new();
    let mut truncated = false;
    for (index, line) in text.lines().enumerate() {
        if line_regex.is_match(line) {
            if matches.len() >= limit {
                truncated = true;
                break;
            }
            matches.push(Match {
                path: display.clone(),
                line: index + 1,
                content: truncate_line(line),
            });
        }
    }
    Ok(Outcome { matches, truncated })
}

/// 排序（路径、行号）并截断到 limit，给出统一的截断判定。
fn finish(mut matches: Vec<Match>, limit: usize, more_pages: bool) -> Outcome {
    matches.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    let truncated = more_pages || matches.len() > limit;
    matches.truncate(limit);
    Outcome { matches, truncated }
}

/// 渲染最终输出（契约格式：`path:line: content` + 截断提示）。
fn render(pattern: &str, root: &Path, outcome: &Outcome, limit: usize) -> ToolResult {
    if outcome.matches.is_empty() {
        return ToolResult::text(format!(
            "No matches found for {pattern:?} in {}",
            root.display()
        ));
    }
    let mut lines: Vec<String> = outcome
        .matches
        .iter()
        .map(|m| format!("{}:{}: {}", m.path, m.line, m.content))
        .collect();
    if outcome.truncated {
        lines.push(format!(
            "[Limit of {limit} matches reached; more matches exist. \
             Refine the pattern/path/glob or raise limit to see more.]"
        ));
    }
    let mut result = ToolResult::text(lines.join("\n"));
    result.details = Some(serde_json::json!({
        "match_count": outcome.matches.len(),
        "truncated": outcome.truncated,
    }));
    result
}

/// 归一化 glob：纯文件名模式（不含 `/`）等价于任意深度匹配。
fn normalize_glob(pattern: &str) -> Result<String, ToolError> {
    let normalized = if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    };
    // 提前校验，非法 glob 直接报错（fff 内部编译失败会静默不匹配）；
    // 与 fff 的编译方式（globset 默认选项）保持一致。
    globset::Glob::new(&normalized)
        .map_err(|e| ToolError::new(format!("Invalid glob {normalized:?}: {e}")))?;
    Ok(normalized)
}

/// 展示路径：搜索根为 `.` 时拼接结果带 `./` 前缀，去掉以保持简洁。
fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    match text.strip_prefix("./") {
        Some(stripped) => stripped.to_string(),
        None => text,
    }
}

/// 截断超长行，避免单行吃掉整个输出预算。
fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        return line.to_string();
    }
    let truncated: String = line.chars().take(MAX_LINE_CHARS).collect();
    format!("{truncated} [line truncated]")
}
