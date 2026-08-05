//! `grep` 工具：ripgrep 语义的内容搜索（正则 / 字面量、gitignore 感知、glob 过滤）。
//!
//! 不依赖外部 rg 二进制：遍历基于 ripgrep 同源的 `ignore` crate（默认遵守
//! .gitignore 并跳过隐藏文件），行匹配、二进制检测与编码处理交给 ripgrep
//! 官方的 `grep-regex` / `grep-searcher` 库。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// 默认最多返回的匹配数。
const DEFAULT_LIMIT: usize = 100;
/// 收集匹配的内部硬上限（防止病态正则撑爆内存）；达到后停止并提示。
const HARD_CAP: usize = 50_000;
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
    /// Also search hidden files and directories
    pub include_hidden: Option<bool>,
}

/// `grep` 工具。
#[derive(Debug, Default, Clone, Copy)]
pub struct GrepTool;

const LABEL: &str = "grep";

const DESCRIPTION: &str = "Search file contents with a regex pattern, ripgrep-style. Respects .gitignore and skips \
         hidden files unless include_hidden is set. Returns matching lines as \"path:line: content\" \
         sorted by file and line number, capped at 100 matches (raise with `limit`). Use `literal` \
         for fixed-string search, `glob` to restrict file types, and `ignore_case` for \
         case-insensitive matching. Prefer this over running grep/rg via bash.";

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
        let mut builder = grep_regex::RegexMatcherBuilder::new();
        builder
            .case_insensitive(params.ignore_case.unwrap_or(false))
            .fixed_strings(params.literal.unwrap_or(false));
        let matcher = builder
            .build(&params.pattern)
            .map_err(|e| ToolError::new(format!("Invalid regex: {e}")))?;
        let glob = params.glob.as_deref().map(build_glob_matcher).transpose()?;
        let root = PathBuf::from(params.path.as_deref().unwrap_or("."));
        let include_hidden = params.include_hidden.unwrap_or(false);
        tracing::debug!(pattern = %params.pattern, root = %root.display(), limit, "grep");

        // 遍历与搜索是阻塞 IO，放到阻塞线程池执行
        let search_root = root.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            search(&search_root, &matcher, glob.as_ref(), include_hidden, limit)
        })
        .await
        .map_err(|e| ToolError::new(format!("Search task failed: {e}")))??;

        if outcome.matches.is_empty() {
            return Ok(ToolResult::text(format!(
                "No matches found for {:?} in {}",
                params.pattern,
                root.display()
            )));
        }
        let mut lines: Vec<String> = outcome
            .matches
            .iter()
            .map(|m| format!("{}:{}: {}", m.path, m.line, m.content))
            .collect();
        if outcome.truncated_by_limit {
            lines.push(format!(
                "[Limit of {limit} matches reached; more matches exist. \
                 Refine the pattern/path/glob or raise limit to see more.]"
            ));
        } else if outcome.truncated_by_cap {
            lines.push(format!(
                "[Stopped after {HARD_CAP} matches. Refine the pattern/path/glob to see more.]"
            ));
        }
        let mut result = ToolResult::text(lines.join("\n"));
        result.details = Some(serde_json::json!({
            "match_count": outcome.matches.len(),
            "truncated": outcome.truncated_by_limit || outcome.truncated_by_cap,
        }));
        Ok(result)
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
    truncated_by_limit: bool,
    truncated_by_cap: bool,
}

/// 收集匹配的 sink；达到硬上限后返回 false 让 searcher 提前停止。
/// 检测到二进制数据时回滚该文件已报的匹配（与 rg 跳过二进制文件的契约一致）。
struct CollectSink<'a> {
    path: &'a str,
    matches: &'a mut Vec<Match>,
    file_start: usize,
    is_binary: bool,
}

impl grep_searcher::Sink for CollectSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        mat: &grep_searcher::SinkMatch<'_>,
    ) -> Result<bool, std::io::Error> {
        let text = String::from_utf8_lossy(mat.bytes());
        let content = text.trim_end_matches(['\n', '\r']);
        self.matches.push(Match {
            path: self.path.to_string(),
            line: usize::try_from(mat.line_number().unwrap_or(0)).unwrap_or(usize::MAX),
            content: truncate_line(content),
        });
        Ok(self.matches.len() < HARD_CAP)
    }

    fn binary_data(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        _binary_byte_offset: u64,
    ) -> Result<bool, std::io::Error> {
        self.is_binary = true;
        Ok(false)
    }
}

/// 遍历 root 下的候选文件并交给 searcher 逐文件搜索，结果按（路径, 行号）排序。
fn search(
    root: &Path,
    matcher: &grep_regex::RegexMatcher,
    glob: Option<&globset::GlobMatcher>,
    include_hidden: bool,
    limit: usize,
) -> Result<Outcome, ToolError> {
    let files = candidate_files(root, glob, include_hidden)?;
    // 与 ripgrep 一致的二进制检测：遇到 NUL 字节即停止该文件
    let mut searcher = grep_searcher::SearcherBuilder::new()
        .binary_detection(grep_searcher::BinaryDetection::quit(b'\x00'))
        .build();
    let mut found = Vec::new();
    let mut truncated_by_cap = false;
    for file in files {
        let display = display_path(&file);
        let file_start = found.len();
        let mut sink = CollectSink {
            path: &display,
            matches: &mut found,
            file_start,
            is_binary: false,
        };
        // 读不了的文件静默跳过；二进制文件回滚已报匹配
        let _ = searcher.search_path(matcher, &file, &mut sink);
        if sink.is_binary {
            sink.matches.truncate(sink.file_start);
        }
        if found.len() >= HARD_CAP {
            truncated_by_cap = true;
            break;
        }
    }
    found.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    let truncated_by_limit = found.len() > limit;
    found.truncate(limit);
    Ok(Outcome {
        matches: found,
        truncated_by_limit,
        truncated_by_cap,
    })
}

/// 枚举待搜索的候选文件：root 为文件时直接返回；否则 gitignore 感知遍历。
fn candidate_files(
    root: &Path,
    glob: Option<&globset::GlobMatcher>,
    include_hidden: bool,
) -> Result<Vec<PathBuf>, ToolError> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(ToolError::new(format!(
            "Path does not exist: {}",
            root.display()
        )));
    }
    let mut files = Vec::new();
    for entry in crate::walk::walk(root, include_hidden) {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            let path = entry.into_path();
            let relative = path.strip_prefix(root).unwrap_or(&path);
            if glob.is_none_or(|m| m.is_match(relative)) {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// 构造 glob 匹配器：纯文件名模式（不含 `/`）等价于任意深度匹配。
fn build_glob_matcher(pattern: &str) -> Result<globset::GlobMatcher, ToolError> {
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

/// 展示路径：遍历 root 为 `.` 时路径带 `./` 前缀，去掉以保持简洁。
fn display_path(file: &Path) -> String {
    let text = file.display().to_string();
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
