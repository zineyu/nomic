//! 工具输出截断：行数与字节数双上限，先到先赢（与 pi 一致：2000 行 / 50KB）。
//! 绝不返回半行（bash 尾部截断的边缘情况除外）。
//!
//! 截断的展示契约（给模型看的说明措辞）由本模块统一所有：调用方拿到
//! [`Truncation`] 后用 [`Truncation::notice`] 生成展示说明，不在模块外
//! 组装截断措辞。

/// 默认最大行数。
pub const DEFAULT_MAX_LINES: usize = 2000;
/// 默认最大字节数（50KB）。
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

/// 截断结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncation {
    /// 截断后的内容
    pub content: String,
    /// 是否发生了截断
    pub truncated: bool,
    /// 命中的上限
    pub truncated_by: Option<TruncatedBy>,
    /// 原始总行数
    pub total_lines: usize,
    /// 原始总字节数
    pub total_bytes: usize,
    /// 输出完整行数
    pub output_lines: usize,
    /// 输出字节数
    pub output_bytes: usize,
    /// 首行是否就超过字节上限（仅头部截断）
    pub first_line_exceeds_limit: bool,
    /// 最后一行是否被部分截断（仅尾部截断的边缘情况）
    pub last_line_partial: bool,
    /// 计算时使用的字节上限（用于展示说明）
    pub max_bytes: usize,
}

/// 命中的截断上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    /// 行数上限
    Lines,
    /// 字节数上限
    Bytes,
}

/// 截断后的继续阅读指引（各工具自己的恢复路径）。
#[derive(Debug)]
pub enum Continuation {
    /// `read`：用 offset 参数继续（下一偏移由 notice 内部推导）
    Offset,
    /// `bash`：完整输出已保存到文件
    FullOutput(String),
}

impl Truncation {
    const fn untruncated(
        content: String,
        total_lines: usize,
        total_bytes: usize,
        max_bytes: usize,
    ) -> Self {
        let output_bytes = content.len();
        Self {
            content,
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes,
            first_line_exceeds_limit: false,
            last_line_partial: false,
            max_bytes,
        }
    }

    /// 展示用截断说明；未截断返回 `None`。
    ///
    /// `start_line` / `total_lines` 为调用方视角的 1-based 行号（`read`
    /// 传文件总行数，`bash` 传输出总行数即 `self.total_lines`）。
    pub fn notice(
        &self,
        start_line: usize,
        total_lines: usize,
        cont: &Continuation,
    ) -> Option<String> {
        if !self.truncated {
            return None;
        }
        let end_line = start_line + self.output_lines.saturating_sub(1);
        let guidance = match cont {
            Continuation::Offset => format!("Use offset={} to continue.", end_line + 1),
            Continuation::FullOutput(path) => format!("Full output: {path}"),
        };
        let notice = if self.last_line_partial {
            format!(
                "[Showing last {} of line {total_lines}. {guidance}]",
                format_size(self.output_bytes)
            )
        } else if self.truncated_by == Some(TruncatedBy::Lines) {
            format!("[Showing lines {start_line}-{end_line} of {total_lines}. {guidance}]")
        } else {
            format!(
                "[Showing lines {start_line}-{end_line} of {total_lines} ({} limit). {guidance}]",
                format_size(self.max_bytes)
            )
        };
        Some(notice)
    }
}

/// 首行即超过字节上限的说明（`read`）；`recovery` 为调用方给出的恢复命令。
pub fn exceeds_notice(line: usize, line_bytes: usize, max_bytes: usize, recovery: &str) -> String {
    format!(
        "[Line {line} is {}, exceeds {} limit. {recovery}]",
        format_size(line_bytes),
        format_size(max_bytes)
    )
}

/// 分割行（与 pi 一致：末尾换行产生的空尾行不计入）。
fn split_lines(content: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// 头部截断：保留前 N 行/字节（`read` 工具）。
pub fn truncate_head(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let total_bytes = content.len();
    let lines: Vec<&str> = split_lines(content);
    let total_lines = lines.len();

    if let Some(first) = lines.first()
        && first.len() > max_bytes
    {
        return Truncation {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            first_line_exceeds_limit: true,
            last_line_partial: false,
            max_bytes,
        };
    }

    let mut output_bytes = 0;
    let mut output_lines = 0;
    let mut truncated_by = None;
    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            truncated_by = Some(TruncatedBy::Lines);
            break;
        }
        // 换行符计入后续行之前（与 pi 一致）
        let line_bytes = line.len() + usize::from(output_lines > 0);
        if output_bytes + line_bytes > max_bytes {
            truncated_by = Some(TruncatedBy::Bytes);
            break;
        }
        output_bytes += line_bytes;
        output_lines += 1;
    }

    if truncated_by.is_none() {
        return Truncation::untruncated(content.to_string(), total_lines, total_bytes, max_bytes);
    }
    Truncation {
        content: lines[..output_lines].join("\n"),
        truncated: true,
        truncated_by,
        total_lines,
        total_bytes,
        output_lines,
        output_bytes,
        first_line_exceeds_limit: false,
        last_line_partial: false,
        max_bytes,
    }
}

/// 尾部截断：保留后 N 行/字节（`bash` 工具）。
pub fn truncate_tail(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let total_bytes = content.len();
    let lines: Vec<&str> = split_lines(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return Truncation::untruncated(content.to_string(), total_lines, total_bytes, max_bytes);
    }

    // 从尾部向前收集完整行（换行符计入前行，与 pi 一致）
    let mut output_bytes = 0;
    let mut kept = 0;
    let mut last_line_partial = false;
    for line in lines.iter().rev() {
        if kept >= max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(kept > 0);
        if output_bytes + line_bytes > max_bytes {
            // 边缘情况：全文最后一行自身就超过字节上限时取该行尾部（唯一允许的半行）
            if kept == 0 {
                kept = 1;
                last_line_partial = true;
            }
            break;
        }
        output_bytes += line_bytes;
        kept += 1;
    }

    let truncated_by = if kept >= max_lines && total_lines > max_lines {
        TruncatedBy::Lines
    } else {
        TruncatedBy::Bytes
    };
    let start = total_lines - kept;
    let kept_content = if last_line_partial {
        let last = lines[total_lines - 1];
        let cut = floor_char_boundary(last, last.len().saturating_sub(max_bytes));
        last[cut..].to_string()
    } else {
        lines[start..].join("\n")
    };

    let output_bytes = kept_content.len();
    Truncation {
        content: kept_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: kept,
        output_bytes,
        first_line_exceeds_limit: false,
        last_line_partial,
        max_bytes,
    }
}

/// 向前找最近的 UTF-8 字符边界。
const fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 格式化字节大小（`50KB`、`1.5MB`）。
// 文件大小远低于 2^52，精度损失可忽略
#[allow(clippy::cast_precision_loss)]
fn format_size(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * KB;
    if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{}KB", bytes / KB)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_no_truncation() {
        let result = truncate_head("a\nb\nc", 10, 100);
        assert!(!result.truncated);
        assert_eq!(result.content, "a\nb\nc");
        assert_eq!(result.total_lines, 3);
    }

    #[test]
    fn head_by_lines() {
        let content = (1..=10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_head(&content, 3, 1_000_000);
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(result.content, "1\n2\n3");
        assert_eq!(result.output_lines, 3);
    }

    #[test]
    fn head_by_bytes() {
        // 与 pi 一致：换行符计入后续行之前，"aaaa\nbbbb" 共 9 字节 <= 10
        let result = truncate_head("aaaa\nbbbb\ncccc", 100, 10);
        assert!(result.truncated);
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(result.content, "aaaa\nbbbb");
    }

    #[test]
    fn head_first_line_exceeds() {
        let result = truncate_head(&"x".repeat(100), 100, 10);
        assert!(result.first_line_exceeds_limit);
    }

    #[test]
    fn tail_by_lines() {
        let content = (1..=10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_tail(&content, 3, 1_000_000);
        assert!(result.truncated);
        assert_eq!(result.content, "8\n9\n10");
        assert_eq!(result.truncated_by, Some(TruncatedBy::Lines));
    }

    #[test]
    fn tail_by_bytes() {
        let result = truncate_tail("aaaa\nbbbb\ncccc", 100, 10);
        assert!(result.truncated);
        assert_eq!(result.content, "bbbb\ncccc");
        assert_eq!(result.truncated_by, Some(TruncatedBy::Bytes));
    }

    #[test]
    fn format_sizes() {
        assert_eq!(format_size(500), "500B");
        assert_eq!(format_size(2048), "2KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0MB");
    }

    #[test]
    fn notice_none_when_untruncated() {
        let result = truncate_head("a\nb", 10, 100);
        assert_eq!(result.notice(1, 2, &Continuation::Offset), None);
    }

    #[test]
    fn notice_by_lines_offset() {
        let content = (1..=10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_head(&content, 3, 1_000_000);
        assert_eq!(
            result.notice(1, 10, &Continuation::Offset).unwrap(),
            "[Showing lines 1-3 of 10. Use offset=4 to continue.]"
        );
    }

    #[test]
    fn notice_by_bytes_offset() {
        let result = truncate_head("aaaa\nbbbb\ncccc", 100, 10);
        assert_eq!(
            result.notice(1, 3, &Continuation::Offset).unwrap(),
            "[Showing lines 1-2 of 3 (10B limit). Use offset=3 to continue.]"
        );
    }

    #[test]
    fn notice_tail_full_output() {
        let content = (1..=10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let result = truncate_tail(&content, 3, 1_000_000);
        let cont = Continuation::FullOutput("/tmp/full.log".to_string());
        assert_eq!(
            result.notice(8, 10, &cont).unwrap(),
            "[Showing lines 8-10 of 10. Full output: /tmp/full.log]"
        );
    }

    #[test]
    fn notice_last_line_partial() {
        // 全文最后一行自身超过字节上限：取该行尾部
        let result = truncate_tail(&format!("a\n{}", "x".repeat(100)), 100, 10);
        assert!(result.last_line_partial);
        let cont = Continuation::FullOutput("/tmp/full.log".to_string());
        assert_eq!(
            result.notice(2, 2, &cont).unwrap(),
            "[Showing last 10B of line 2. Full output: /tmp/full.log]"
        );
    }

    #[test]
    fn exceeds_notice_formats() {
        assert_eq!(
            exceeds_notice(
                5,
                100 * 1024,
                50 * 1024,
                "Use bash: sed -n '5p' f | head -c 51200"
            ),
            "[Line 5 is 100KB, exceeds 50KB limit. Use bash: sed -n '5p' f | head -c 51200]"
        );
    }
}
