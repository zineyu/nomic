//! 工具输出截断：行数与字节数双上限，先到先赢（与 pi 一致：2000 行 / 50KB）。
//! 绝不返回半行（bash 尾部截断的边缘情况除外）。

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
}

/// 命中的截断上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    /// 行数上限
    Lines,
    /// 字节数上限
    Bytes,
}

impl Truncation {
    const fn untruncated(content: String, total_lines: usize, total_bytes: usize) -> Self {
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
        }
    }
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
        return Truncation::untruncated(content.to_string(), total_lines, total_bytes);
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
    }
}

/// 尾部截断：保留后 N 行/字节（`bash` 工具）。
pub fn truncate_tail(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let total_bytes = content.len();
    let lines: Vec<&str> = split_lines(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return Truncation::untruncated(content.to_string(), total_lines, total_bytes);
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
pub fn format_size(bytes: usize) -> String {
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
}
