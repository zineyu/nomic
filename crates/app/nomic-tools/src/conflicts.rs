//! `:conflicts` 选择器的冲突区域提取（ADR-0040 §协议目录 / 选择器语法）。
//!
//! 三路合并语义：`content` = ours、`base` = 共同祖先（缺省 = 零冲突 fast
//! path）、`theirs` = 对侧。冲突检测以 `git merge-file --diff3` 为后端
//!（与 git 合并语义对齐），再把合并输出中的冲突块映射回 **ours 的行号**
//! （`[start, end]`，1-indexed 含端点；`[n, n]` 且 ours 段为空 = 零宽插入）。
//!
//! 映射原理：剥离合并输出的冲突标记与 base/theirs 段得到 `M'`（= ours +
//! theirs 的干净变更），`similar` 对齐 ours ↔ `M'`；冲突块在 `M'` 中的行
//! 区间据此换算回 ours。未安装 git 时返回 [`ConflictsError::GitUnavailable`]。

use std::process::Stdio;

use std::io::Write as _;

use similar::{DiffOp, TextDiff};
use thiserror::Error;

/// 冲突提取错误。
#[derive(Debug, Error)]
pub enum ConflictsError {
    /// 系统无 git（或无法启动）
    #[error(
        "The :conflicts selector requires git, but it could not be executed ({reason}). \
         Install git or read the files without :conflicts."
    )]
    GitUnavailable {
        /// 底层错误
        reason: String,
    },
    /// git 执行失败（非冲突性失败）
    #[error("git merge-file failed: {0}")]
    GitFailed(String),
}

/// 一个冲突区域（ours 行号，1-indexed 含端点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictRange {
    /// 起始行
    pub start: usize,
    /// 结束行（含）；`start == end` 且 ours 段为空 = 零宽插入点
    pub end: usize,
    /// ours 段是否为空（零宽插入：对侧在此有新内容而本侧无）
    pub empty: bool,
}

/// 提取 ours 与 theirs（相对共同祖先 base）的冲突区域。
///
/// `base` 为 `None` 时是 fast path：无共同祖先语义上无三方冲突，返回空。
pub async fn find_conflicts(
    ours: &str,
    base: Option<&str>,
    theirs: &str,
) -> Result<Vec<ConflictRange>, ConflictsError> {
    let Some(base) = base else {
        return Ok(Vec::new());
    };
    let merged = merge_file_diff3(base, ours, theirs).await?;
    Ok(parse_conflicts(ours, &merged))
}

/// `git merge-file -p --diff3`：三方文本写临时文件后合并，返回合并输出。
/// 退出码语义：0 = 无冲突；N > 0 = N 个冲突（输出仍有效）；其它为错误。
async fn merge_file_diff3(base: &str, ours: &str, theirs: &str) -> Result<String, ConflictsError> {
    let mut files = Vec::with_capacity(3);
    for content in [base, ours, theirs] {
        let mut file = tempfile::NamedTempFile::new()
            .map_err(|e| ConflictsError::GitFailed(format!("could not create temp file: {e}")))?;
        file.write_all(content.as_bytes())
            .map_err(|e| ConflictsError::GitFailed(format!("could not write temp file: {e}")))?;
        files.push(file);
    }
    // 参数序：<current>(ours) <base> <other>(theirs)
    let output = tokio::process::Command::new("git")
        .args([
            "merge-file",
            "-p",
            "--diff3",
            "-L",
            "ours",
            "-L",
            "base",
            "-L",
            "theirs",
        ])
        .arg(files[1].path())
        .arg(files[0].path())
        .arg(files[2].path())
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| ConflictsError::GitUnavailable {
            reason: e.to_string(),
        })?;
    let merged = String::from_utf8_lossy(&output.stdout).into_owned();
    match output.status.code() {
        Some(code) if code >= 0 => Ok(merged), // 0 = 干净；N = 冲突数
        _ => Err(ConflictsError::GitFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        )),
    }
}

/// 解析 diff3 合并输出，把冲突块映射回 ours 行号。
fn parse_conflicts(ours: &str, merged: &str) -> Vec<ConflictRange> {
    // 第一遍：剥出 M'（ours 段 + 干净区），记录各冲突块在 M' 的行区间
    let mut mprime: Vec<&str> = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new(); // M' 行区间（1-indexed 含端点）
    let mut lines = merged.split('\n');
    while let Some(line) = lines.next() {
        if line.starts_with("<<<<<<<") {
            let start = mprime.len() + 1;
            // ours 段：直到 |||||||（diff3 基线标记）或 =======
            for line in lines.by_ref() {
                if line.starts_with("|||||||") || line.starts_with("=======") {
                    break;
                }
                mprime.push(line);
            }
            spans.push((start, mprime.len()));
            // 跳过剩余段（base 段到 =======；theirs 段到 >>>>>>>）
            let mut seen_separator = false;
            for line in lines.by_ref() {
                if line.starts_with("=======") {
                    seen_separator = true;
                } else if seen_separator && line.starts_with(">>>>>>>") {
                    break;
                }
            }
        } else {
            mprime.push(line);
        }
    }
    if spans.is_empty() {
        return Vec::new();
    }

    // 第二遍：ours ↔ M' 对齐（M' 中来自 ours 的行全部相等映射）
    let mprime_text = mprime.join("\n");
    let diff = TextDiff::from_lines(ours, &mprime_text);
    // mprime_line(1-indexed) → ours_line(1-indexed)
    let mut map = vec![0usize; mprime.len() + 1];
    for op in diff.ops() {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for i in 0..*len {
                map[new_index + i + 1] = old_index + i + 1;
            }
        }
    }
    // 末行哨兵：零宽插入在文件尾时取「最后一行」
    let ours_total = ours.split('\n').count();
    let line_at_or_after = |mprime_line: usize| -> usize {
        // 找 M' 中 >= mprime_line 的首个映射行
        (mprime_line..=mprime.len())
            .find_map(|line| (map[line] > 0).then(|| map[line]))
            .unwrap_or(ours_total)
    };

    spans
        .iter()
        .map(|&(start, end)| {
            if start <= end {
                ConflictRange {
                    start: map[start],
                    end: map[end],
                    empty: false,
                }
            } else {
                // 零宽插入：定位到插入点所在的 ours 行
                let line = line_at_or_after(start);
                ConflictRange {
                    start: line,
                    end: line,
                    empty: true,
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_base_is_zero_conflict_fast_path() {
        let ranges = find_conflicts("ours", None, "theirs")
            .await
            .expect("fast path");
        assert!(ranges.is_empty());
    }

    #[tokio::test]
    async fn clean_merge_has_no_conflicts() {
        let base = "a\nb\nc\n";
        let ours = "A\nb\nc\n"; // 改第 1 行
        let theirs = "a\nb\nC\n"; // 改第 3 行（不冲突）
        let ranges = find_conflicts(ours, Some(base), theirs)
            .await
            .expect("merge");
        assert!(ranges.is_empty());
    }

    #[tokio::test]
    async fn overlapping_edits_conflict_in_ours_coordinates() {
        let base = "a\nb\nc\n";
        let ours = "a\nB-ours\nc\nd\n"; // 改第 2 行 + 末尾加一行
        let theirs = "a\nB-theirs\nc\n"; // 改第 2 行（冲突）
        let ranges = find_conflicts(ours, Some(base), theirs)
            .await
            .expect("merge");
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            (ranges[0].start, ranges[0].end, ranges[0].empty),
            (2, 2, false)
        );
    }

    #[tokio::test]
    async fn theirs_clean_changes_do_not_shift_ours_line_numbers() {
        // theirs 在第 1 行有干净插入，冲突在 ours 第 4 行：
        // 映射必须回到 ours 坐标（4），而不是合并输出的坐标（5）
        let base = "a\nb\nc\nd\n";
        let ours = "a\nb\nc\nD-ours\n";
        let theirs = "inserted\na\nb\nc\nD-theirs\n";
        let ranges = find_conflicts(ours, Some(base), theirs)
            .await
            .expect("merge");
        assert_eq!(ranges.len(), 1);
        assert_eq!((ranges[0].start, ranges[0].end), (4, 4));
        assert!(!ranges[0].empty);
    }

    #[tokio::test]
    async fn zero_width_insertion_when_ours_side_is_empty() {
        // 双方在同一基线位置插入：ours 在 b/c 之间插了一行、theirs 插了另一行
        let base = "a\nb\nc\n";
        let ours = "a\nb\nc-ours-extra\nc\n";
        let theirs = "a\nb\ntheirs-extra\nc\n";
        let ranges = find_conflicts(ours, Some(base), theirs)
            .await
            .expect("merge");
        assert_eq!(ranges.len(), 1);
        assert!(!ranges[0].empty);
        assert_eq!((ranges[0].start, ranges[0].end), (3, 3));

        // ours 删除 b、theirs 修改 b：删除/修改冲突，ours 侧零宽
        let ours = "a\nc\n";
        let theirs_mod = "a\nB-theirs\nc\n";
        let ranges = find_conflicts(ours, Some(base), theirs_mod)
            .await
            .expect("merge");
        assert_eq!(ranges.len(), 1);
        assert!(ranges[0].empty, "{:?}", ranges[0]);
        // 插入点定位到「其后首个 ours 行」（删除 b 后 ours 第 2 行是 "c"）
        assert_eq!((ranges[0].start, ranges[0].end), (2, 2));
    }
}
