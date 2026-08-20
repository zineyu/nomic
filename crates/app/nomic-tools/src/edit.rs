//! `edit` 工具：多处精确替换（契约与 pi 一致）。
//!
//! - `edits[]` 对**原始文件**匹配（非增量），禁止重叠/嵌套
//! - 精确匹配失败时按行模糊匹配（归一化：行尾空白、智能引号、Unicode 破折号/空格）
//! - 保留 BOM 与 CRLF；返回 unified diff/patch 作为 details

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::Deserialize;
use similar::TextDiff;
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

use crate::mutation_queue::lock_path;

/// 单处替换。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditBlock {
    /// Exact text for one targeted replacement. It must be unique in the original file
    /// and must not overlap with any other edits[].oldText in the same call.
    #[serde(rename = "oldText")]
    pub old_text: String,
    /// Replacement text for this targeted edit.
    #[serde(rename = "newText")]
    pub new_text: String,
}

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditParams {
    /// Path to the file to edit (relative or absolute)
    pub path: String,
    /// One or more targeted replacements. Each edit is matched against the original file,
    /// not incrementally. Do not include overlapping or nested edits. If two changes touch
    /// the same block or nearby lines, merge them into one edit instead.
    pub edits: Vec<EditBlock>,
}

/// `edit` 工具。
#[derive(Debug, Default, Clone)]
pub struct EditTool {
    /// 相对路径的解析基准（workspace 严格归属；空句柄 = 进程 cwd）
    base: crate::base::BaseDir,
}

impl EditTool {
    /// 创建以进程 cwd 为基准的 edit 工具。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置固定基准目录：相对路径以它解析（workspace 严格归属）。
    #[must_use]
    pub fn with_base_dir(mut self, base_dir: Option<std::path::PathBuf>) -> Self {
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

const LABEL: &str = "edit";

const DESCRIPTION: &str = "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, \
         non-overlapping region of the original file. If two changes affect the same block or nearby \
         lines, merge them into one edit instead of emitting overlapping edits. Do not include large \
         unchanged regions just to connect distant changes.";

#[async_trait]
impl AgentTool for EditTool {
    type Params = EditParams;

    fn name(&self) -> &'static str {
        "edit"
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
        if params.edits.is_empty() {
            return Err(ToolError::new(
                "Edit tool input is invalid. edits must contain at least one replacement.",
            ));
        }
        let base = self.base.snapshot();
        let path = crate::base::resolve(base.as_deref(), &params.path);
        let _guard = lock_path(&path).await;
        if cancel.is_cancelled() {
            return Err(ToolError::new("Operation aborted"));
        }

        let raw = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::new(format!("Could not edit file: {}. {e}", params.path)))?;
        let raw = String::from_utf8(raw).map_err(|_| {
            ToolError::new(format!(
                "Could not edit file: {}. File is not valid UTF-8.",
                params.path
            ))
        })?;

        let (bom, without_bom) = strip_bom(&raw);
        let line_ending = detect_line_ending(without_bom);
        let content = normalize_to_lf(without_bom);

        let base_content = content.clone();
        let new_content = apply_edits(&content, &params.edits, &params.path)?;
        if cancel.is_cancelled() {
            return Err(ToolError::new("Operation aborted"));
        }

        let final_content = format!("{bom}{}", restore_line_endings(&new_content, line_ending));
        tokio::fs::write(path, final_content)
            .await
            .map_err(|e| ToolError::new(format!("Could not edit file: {}. {e}", params.path)))?;
        if cancel.is_cancelled() {
            return Err(ToolError::new("Operation aborted"));
        }

        let (diff, first_changed_line) = generate_diff(&base_content, &new_content);
        tracing::debug!(path = %params.path, blocks = params.edits.len(), "edit applied");
        let mut result = ToolResult::text(format!(
            "Successfully replaced {} block(s) in {}.",
            params.edits.len(),
            params.path
        ));
        result.details = Some(serde_json::json!({
            "diff": diff,
            "patch": generate_patch(&params.path, &base_content, &new_content),
            "first_changed_line": first_changed_line,
        }));
        Ok(result)
    }
}

// ── 行尾与 BOM ───────────────────────────────────────────────────────────────

/// 剥离 BOM，返回 (BOM, 剩余内容)。
fn strip_bom(content: &str) -> (&str, &str) {
    const BOM: &str = "\u{FEFF}";
    content
        .strip_prefix(BOM)
        .map_or(("", content), |rest| (BOM, rest))
}

/// 检测文件的主要行尾（与 pi 一致：首个 \n 属于 \r\n 即 CRLF）。
fn detect_line_ending(content: &str) -> &'static str {
    let crlf = content.find("\r\n");
    let lf = content.find('\n');
    match (crlf, lf) {
        (Some(c), Some(l)) if c < l => "\r\n",
        _ => "\n",
    }
}

fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

// ── 匹配与替换 ───────────────────────────────────────────────────────────────

/// 模糊匹配归一化（与 pi 一致）：NFKC、行尾空白、智能引号、Unicode 破折号/空格。
fn normalize_for_fuzzy_match(text: &str) -> String {
    text.nfkc()
        .collect::<String>()
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .replace(['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'], "'")
        .replace(['\u{201C}', '\u{201D}', '\u{201E}', '\u{201F}'], "\"")
        .replace(
            [
                '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
            ],
            "-",
        )
        .replace(
            [
                '\u{00A0}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
                '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}',
            ],
            " ",
        )
}

/// 一处替换的匹配结果：在 LF 内容中的字节区间。
#[derive(Debug, Clone, Copy)]
struct MatchSpan {
    start: usize,
    end: usize,
}

/// 对 LF 归一化后的内容应用所有编辑（对原始内容匹配，禁止重叠）。
fn apply_edits(content: &str, edits: &[EditBlock], path: &str) -> Result<String, ToolError> {
    let mut spans: Vec<MatchSpan> = Vec::with_capacity(edits.len());
    for (i, edit) in edits.iter().enumerate() {
        spans.push(find_match(content, &edit.old_text, path, i)?);
    }

    // 重叠校验
    let mut sorted = spans.clone();
    sorted.sort_by_key(|s| s.start);
    for pair in sorted.windows(2) {
        if pair[0].end > pair[1].start {
            return Err(ToolError::new(format!(
                "Could not edit file: {path}. edits[].oldText regions overlap; \
                 merge edits that touch the same block into one edit."
            )));
        }
    }

    // 从后往前应用，保持偏移有效
    let mut result = content.to_string();
    let mut ordered: Vec<(&EditBlock, MatchSpan)> = edits.iter().zip(spans).collect();
    ordered.sort_by_key(|(_, span)| std::cmp::Reverse(span.start));
    for (edit, span) in ordered {
        result.replace_range(span.start..span.end, &edit.new_text);
    }
    Ok(result)
}

/// 定位一处替换：先精确匹配，失败后按行模糊匹配；均要求唯一。
fn find_match(
    content: &str,
    old_text: &str,
    path: &str,
    edit_index: usize,
) -> Result<MatchSpan, ToolError> {
    let ordinal = edit_index + 1;

    // 精确匹配
    let exact: Vec<usize> = content.match_indices(old_text).map(|(i, _)| i).collect();
    match exact.len() {
        1 => {
            return Ok(MatchSpan {
                start: exact[0],
                end: exact[0] + old_text.len(),
            });
        }
        n if n > 1 => {
            return Err(ToolError::new(format!(
                "Could not edit file: {path}. edits[{ordinal}].oldText matches {n} locations. \
                 Provide more surrounding context to make the match unique."
            )));
        }
        _ => {}
    }

    // 模糊匹配（行级，归一化后比较；行与行一一对应，映射回原始行区间）
    let content_lines: Vec<&str> = content.split('\n').collect();
    let normalized_lines: Vec<String> = content_lines
        .iter()
        .map(|l| normalize_for_fuzzy_match(l))
        .collect();
    let old_lines: Vec<String> = normalize_for_fuzzy_match(old_text)
        .split('\n')
        .map(str::to_string)
        .collect();
    if old_lines.is_empty() || old_lines.iter().all(String::is_empty) {
        return Err(ToolError::new(format!(
            "Could not edit file: {path}. edits[{ordinal}].oldText is empty."
        )));
    }

    let mut matches = Vec::new();
    if old_lines.len() <= normalized_lines.len() {
        for start in 0..=(normalized_lines.len() - old_lines.len()) {
            if normalized_lines[start..start + old_lines.len()] == old_lines[..] {
                matches.push(start);
            }
        }
    }

    match matches.len() {
        1 => {
            let start_line = matches[0];
            let end_line = start_line + old_lines.len() - 1;
            let byte_start = line_byte_offset(&content_lines, start_line);
            let byte_end =
                line_byte_offset(&content_lines, end_line) + content_lines[end_line].len();
            Ok(MatchSpan {
                start: byte_start,
                end: byte_end,
            })
        }
        0 => Err(ToolError::new(format!(
            "Could not edit file: {path}. Could not find the exact match for edits[{ordinal}].oldText. \
             Read the file first and copy the exact text."
        ))),
        n => Err(ToolError::new(format!(
            "Could not edit file: {path}. edits[{ordinal}].oldText matches {n} locations (fuzzy). \
             Provide more surrounding context to make the match unique."
        ))),
    }
}

/// 第 `line` 行在内容中的起始字节偏移。
fn line_byte_offset(lines: &[&str], line: usize) -> usize {
    lines[..line].iter().map(|l| l.len() + 1).sum()
}

// ── diff ─────────────────────────────────────────────────────────────────────

/// 生成易读 diff 与首个变更行号。
fn generate_diff(base: &str, new: &str) -> (String, Option<usize>) {
    let diff = TextDiff::from_lines(base, new);
    let mut output = String::new();
    let mut first_changed_line = None;
    let mut line_no = 1;
    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            similar::ChangeTag::Delete => "-",
            similar::ChangeTag::Insert => "+",
            similar::ChangeTag::Equal => " ",
        };
        if change.tag() != similar::ChangeTag::Equal && first_changed_line.is_none() {
            first_changed_line = Some(line_no);
        }
        if change.tag() != similar::ChangeTag::Insert {
            line_no += 1;
        }
        output.push_str(sign);
        output.push_str(change.value());
    }
    (output, first_changed_line)
}

/// 生成 unified patch。
fn generate_patch(path: &str, base: &str, new: &str) -> String {
    TextDiff::from_lines(base, new)
        .unified_diff()
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(old: &str, new: &str) -> EditBlock {
        EditBlock {
            old_text: old.to_string(),
            new_text: new.to_string(),
        }
    }

    #[test]
    fn exact_single_match() {
        let result = apply_edits("hello world", &[block("world", "rust")], "f").expect("ok");
        assert_eq!(result, "hello rust");
    }

    #[test]
    fn multiple_edits_applied_against_original() {
        let result = apply_edits("a b c d", &[block("a", "A"), block("c", "C")], "f").expect("ok");
        assert_eq!(result, "A b C d");
    }

    #[test]
    fn non_unique_match_errors() {
        let err = apply_edits("a a a", &[block("a", "b")], "f").unwrap_err();
        assert!(err.to_string().contains("3 locations"), "{err}");
    }

    #[test]
    fn overlapping_edits_error() {
        let err = apply_edits("abcdef", &[block("abc", "x"), block("bcd", "y")], "f").unwrap_err();
        assert!(err.to_string().contains("overlap"), "{err}");
    }

    #[test]
    fn missing_match_errors() {
        let err = apply_edits("hello", &[block("nonexistent", "x")], "f").unwrap_err();
        assert!(err.to_string().contains("Could not find"), "{err}");
    }

    #[test]
    fn fuzzy_match_smart_quotes() {
        // 文件含智能引号，oldText 用 ASCII 引号也能匹配
        let content = "say \u{201C}hello\u{201D} loudly";
        let result = apply_edits(
            content,
            &[block("say \"hello\" loudly", "whisper \"hello\"")],
            "f",
        )
        .expect("ok");
        assert_eq!(result, "whisper \"hello\"");
    }

    #[test]
    fn fuzzy_match_trailing_whitespace() {
        let content = "line one   \nline two";
        let result = apply_edits(content, &[block("line one\nline two", "both")], "f").expect("ok");
        assert_eq!(result, "both");
    }

    #[test]
    fn fuzzy_multiple_matches_error() {
        // 行尾空白不同使精确匹配为 0，模糊匹配命中 2 处
        let content = "foo  \nbar\nx\nfoo \nbar";
        let err = apply_edits(content, &[block("foo\nbar", "baz")], "f").unwrap_err();
        assert!(err.to_string().contains("2 locations (fuzzy)"), "{err}");
    }

    #[test]
    fn crlf_detected_and_restored() {
        assert_eq!(detect_line_ending("a\r\nb\r\n"), "\r\n");
        assert_eq!(detect_line_ending("a\nb"), "\n");
        let normalized = normalize_to_lf("a\r\nb\r\n");
        assert_eq!(normalized, "a\nb\n");
        assert_eq!(restore_line_endings(&normalized, "\r\n"), "a\r\nb\r\n");
    }

    #[test]
    fn bom_stripped() {
        assert_eq!(strip_bom("\u{FEFF}abc"), ("\u{FEFF}", "abc"));
        assert_eq!(strip_bom("abc"), ("", "abc"));
    }
}
