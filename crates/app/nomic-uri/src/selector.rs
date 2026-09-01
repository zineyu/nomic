//! URI 尾挂选择器：`:N`、`:A-B`、`:A+C`、`:N-`（开放结尾）、多段
//! `:5-10,20-30`、`..` 作为 `-` 的宽容别名、`:raw`、`:conflicts`，以及
//! 复合 `:raw:50-100` / `:50-100:raw`（恰好一个 range + 一个 raw）。
//!
//! 与 oh-my-pi 的三态哲学一致：
//! - **文件系统路径**：nomic 不使用尾挂选择器（read 走 offset/limit 参数）；
//! - **内部 URI**：激进剥离——scheme 之后的选择器形尾块一律剥下，交由
//!   [`parse_selector`] 统一校验，畸形即报错（fail-closed，不静默放宽）。

use thiserror::Error;

/// 选择器链判定：一个以上选择器块以 `:` 相连。
///
/// 供 `parse.rs` 的 opaque URI 防误判使用（`Makefile:12` / `notes:raw`
/// 不是 URI）。块语法：`raw | conflicts | -?\d+([-+]\d+)?(,\d+([-+]\d+)?)*`。
pub fn is_selector_chain(input: &str) -> bool {
    !input.is_empty() && input.split(':').all(is_selector_chunk)
}

fn is_selector_chunk(chunk: &str) -> bool {
    if chunk.is_empty() {
        return false;
    }
    if chunk.eq_ignore_ascii_case("raw") || chunk.eq_ignore_ascii_case("conflicts") {
        return true;
    }
    // -?\d+([-+]\d+)?(,\d+([-+]\d+)?)*
    let mut parts = chunk.split(',');
    let Some(first) = parts.next() else {
        return false;
    };
    is_chain_range_part(first, true) && parts.all(|part| is_chain_range_part(part, false))
}

fn is_chain_range_part(part: &str, allow_leading_minus: bool) -> bool {
    let s = if allow_leading_minus {
        part.strip_prefix('-').unwrap_or(part)
    } else {
        part
    };
    let digit_len = s.bytes().take_while(u8::is_ascii_digit).count();
    if digit_len == 0 {
        return false;
    }
    let rest = &s[digit_len..];
    if rest.is_empty() {
        return true;
    }
    let Some(after_sign) = rest.strip_prefix('-').or_else(|| rest.strip_prefix('+')) else {
        return false;
    };
    !after_sign.is_empty() && after_sign.bytes().all(|b| b.is_ascii_digit())
}

/// 选择器解析错误（fail-closed：看似选择器但语法非法即报错）。
#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct SelectorError(pub String);

/// 一行范围（1-indexed；`end: None` = 开放结尾）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    /// 起始行（>= 1）
    pub start: usize,
    /// 结束行（含）；`None` = 到末尾
    pub end: Option<usize>,
}

/// 解析后的选择器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSelector {
    /// 无选择器
    None,
    /// `:raw`——原文输出（禁用结构摘要/行前缀等加工）
    Raw,
    /// `:conflicts`——扫描 git 合并冲突区域
    Conflicts,
    /// 行范围（可多段；`raw` = 复合形式中的 `:raw`）
    Lines {
        /// 排序合并前的范围列表（至少一段）
        ranges: Vec<LineRange>,
        /// 是否同时要求 raw 输出
        raw: bool,
    },
}

impl ParsedSelector {
    /// 是否要求 raw 输出（单独 `:raw` 或复合中的 raw）。
    #[must_use]
    pub const fn is_raw(&self) -> bool {
        matches!(self, Self::Raw) || matches!(self, Self::Lines { raw, .. } if *raw)
    }

    /// 单段范围转 read 工具的 offset/limit 参数（1-indexed offset）。
    /// 多段范围返回 `None`——调用方必须自行按段切片。
    #[must_use]
    pub fn to_offset_limit(&self) -> Option<(Option<usize>, Option<usize>)> {
        match self {
            Self::Lines { ranges, .. } if ranges.len() == 1 => {
                let range = ranges[0];
                Some((
                    Some(range.start),
                    range.end.map(|end| end - range.start + 1),
                ))
            }
            _ => None,
        }
    }
}

/// 剥离内部 URI 尾部的选择器链，返回 `(干净 URI, 选择器串)`。
///
/// 激进而迭代：从右往左剥所有「选择器形」尾块（含 `:-N` 这类常见畸形），
/// 把干净 URI 交给协议 handler，选择器错误由 [`parse_selector`] 统一报告，
/// 而不是让 handler 抛误导性的 "host invalid"。非 URI 输入原样返回。
pub fn split_uri_selector(input: &str) -> (String, Option<String>) {
    // 选择器挂在 path 尾部、query 之前：先摘 query，剥离后重新拼回
    let (head, query) = match input.find('?') {
        Some(index) => (&input[..index], &input[index..]),
        None => (input, ""),
    };
    let Some(scheme_len) = hierarchical_scheme_end(head) else {
        return (input.to_string(), None);
    };
    let mut path = head;
    let mut chunks: Vec<&str> = Vec::new();
    while let Some(colon) = path.rfind(':') {
        // 不越过 scheme 分隔符 `://`
        if colon < scheme_len {
            break;
        }
        let tail = &path[colon + 1..];
        if !is_permissive_selector_chunk(tail) {
            break;
        }
        chunks.push(tail);
        path = &path[..colon];
    }
    if chunks.is_empty() {
        return (input.to_string(), None);
    }
    chunks.reverse();
    (format!("{path}{query}"), Some(chunks.join(":")))
}

/// `scheme://` 分隔符末尾的偏移（即 host 起点）。
fn hierarchical_scheme_end(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
        return None;
    }
    let mut end = 1;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'+' | b'-' | b'.'))
    {
        end += 1;
    }
    input[end..].strip_prefix("://").map(|_| end + 3)
}

/// 宽松选择器块：合法形状 + 常见畸形（`:-10`），剥离阶段从宽，
/// 严格校验留给 [`parse_selector`]。
fn is_permissive_selector_chunk(chunk: &str) -> bool {
    if chunk.is_empty() {
        return false;
    }
    if chunk.eq_ignore_ascii_case("raw") || chunk.eq_ignore_ascii_case("conflicts") {
        return true;
    }
    // `-\d+([-+]\d+)?`（畸形负数）
    if let Some(rest) = chunk.strip_prefix('-') {
        let digit_len = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digit_len > 0 && digit_len == rest.len() {
            return true;
        }
    }
    parse_line_ranges(chunk).is_some()
}

/// 解析选择器串（[`split_uri_selector`] 剥出的部分）。
///
/// 接受集：单个块（range 列表 / `raw` / `conflicts`），或恰好
/// 「一个 range 块 + 一个 `raw` 块」的复合。其余一律报错——畸形输入
/// 报错而非静默放宽，保证各工具行为一致。
pub fn parse_selector(sel: &str) -> Result<ParsedSelector, SelectorError> {
    if sel.is_empty() {
        return Ok(ParsedSelector::None);
    }
    let invalid = || {
        SelectorError(format!(
            "Invalid selector ':{sel}'. Use :N, :N-M, :N+K, :N- (open-ended), a comma-separated \
             list of ranges, :raw, :conflicts, or a range combined with raw (e.g. :raw:50-100)."
        ))
    };

    if sel.contains(':') {
        let chunks: Vec<&str> = sel.split(':').collect();
        if chunks.len() == 2 {
            let [a, b] = [chunks[0], chunks[1]];
            let (range_chunk, raw_ok) = if a.eq_ignore_ascii_case("raw") {
                (b, true)
            } else if b.eq_ignore_ascii_case("raw") {
                (a, true)
            } else {
                ("", false)
            };
            if raw_ok && let Some(ranges) = parse_line_ranges(range_chunk) {
                return Ok(ParsedSelector::Lines { ranges, raw: true });
            }
        }
        // 任何含 `:` 的剩余组合都不在接受集内
        return Err(invalid());
    }

    if sel.eq_ignore_ascii_case("raw") {
        return Ok(ParsedSelector::Raw);
    }
    if sel.eq_ignore_ascii_case("conflicts") {
        return Ok(ParsedSelector::Conflicts);
    }
    if let Some(ranges) = parse_line_ranges(sel) {
        return Ok(ParsedSelector::Lines { ranges, raw: false });
    }
    Err(invalid())
}

/// 解析逗号分隔的行范围列表。语法（1-indexed，大小写不敏感的 `L` 前缀）：
/// `N` / `N-M` / `N..M`（`..` 归一化为 `-`）/ `N+K`（K 行）/ `N-` `N..`
/// （开放结尾）。校验：`N >= 1`；`K >= 1`；`M >= N`。多段按起点排序。
fn parse_line_ranges(input: &str) -> Option<Vec<LineRange>> {
    let mut ranges = Vec::new();
    for part in input.split(',') {
        ranges.push(parse_line_range_part(part)?);
    }
    ranges.sort_by_key(|range| range.start);
    Some(ranges)
}

fn parse_line_range_part(part: &str) -> Option<LineRange> {
    let s = part.strip_prefix('L').unwrap_or(part);
    let digit_len = s.bytes().take_while(u8::is_ascii_digit).count();
    if digit_len == 0 {
        return None;
    }
    let start: usize = s[..digit_len].parse().ok()?;
    if start == 0 {
        return None; // 行号 1-indexed；:0 非法
    }
    let rest = &s[digit_len..];
    if rest.is_empty() {
        return Some(LineRange {
            start,
            end: Some(start),
        });
    }
    let (sep, after) = if let Some(after) = rest.strip_prefix("..") {
        ("..", after)
    } else if let Some(after) = rest.strip_prefix('-') {
        ("-", after)
    } else {
        let after = rest.strip_prefix('+')?;
        ("+", after)
    };
    let after = after.strip_prefix('L').unwrap_or(after);
    if after.is_empty() {
        // 开放结尾仅 `-` / `..` 合法；`N+` 非法
        return match sep {
            "+" => None,
            _ => Some(LineRange { start, end: None }),
        };
    }
    if !after.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: usize = after.parse().ok()?;
    if sep == "+" {
        if value == 0 {
            return None; // +0 非法
        }
        return Some(LineRange {
            start,
            end: Some(start + value - 1),
        });
    }
    if value < start {
        return None; // 结束 >= 起点
    }
    Some(LineRange {
        start,
        end: Some(value),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(input: &str) -> (String, Option<String>) {
        split_uri_selector(input)
    }

    #[test]
    fn splits_selector_off_uri() {
        assert_eq!(
            split("artifact://3:100-200"),
            ("artifact://3".to_string(), Some("100-200".to_string()))
        );
        assert_eq!(
            split("artifact://3:raw:1-3000"),
            ("artifact://3".to_string(), Some("raw:1-3000".to_string()))
        );
        assert_eq!(
            split("skill://pdf:raw"),
            ("skill://pdf".to_string(), Some("raw".to_string()))
        );
        // 非选择器尾巴不剥（skill 名里的冒号）
        assert_eq!(
            split("skill://plugin:name"),
            ("skill://plugin:name".to_string(), None)
        );
        // 非 URI 原样返回
        assert_eq!(
            split("src/main.rs:10-20"),
            ("src/main.rs:10-20".to_string(), None)
        );
        // 畸形负数块也剥下（交给 parse_selector 报错）
        assert_eq!(
            split("artifact://3:-100"),
            ("artifact://3".to_string(), Some("-100".to_string()))
        );
        // 不越过 scheme 分隔符
        assert_eq!(split("local://"), ("local://".to_string(), None));
        // 选择器在 query 之前：剥离后 query 保留在干净 URI 上
        assert_eq!(
            split("local://a.md:conflicts?theirs=b.md"),
            (
                "local://a.md?theirs=b.md".to_string(),
                Some("conflicts".to_string())
            )
        );
        assert_eq!(
            split("artifact://3:raw:1-9?x=1"),
            ("artifact://3?x=1".to_string(), Some("raw:1-9".to_string()))
        );
    }

    #[test]
    fn parses_single_forms() {
        assert_eq!(parse_selector("").unwrap(), ParsedSelector::None);
        assert_eq!(parse_selector("raw").unwrap(), ParsedSelector::Raw);
        assert_eq!(parse_selector("RAW").unwrap(), ParsedSelector::Raw);
        assert_eq!(
            parse_selector("conflicts").unwrap(),
            ParsedSelector::Conflicts
        );
        assert_eq!(
            parse_selector("50").unwrap(),
            ParsedSelector::Lines {
                ranges: vec![LineRange {
                    start: 50,
                    end: Some(50)
                }],
                raw: false
            }
        );
        assert_eq!(
            parse_selector("50-100").unwrap(),
            ParsedSelector::Lines {
                ranges: vec![LineRange {
                    start: 50,
                    end: Some(100)
                }],
                raw: false
            }
        );
        // 开放结尾
        assert_eq!(
            parse_selector("50-").unwrap(),
            ParsedSelector::Lines {
                ranges: vec![LineRange {
                    start: 50,
                    end: None
                }],
                raw: false
            }
        );
        // +K 计数转闭区间
        assert_eq!(
            parse_selector("50+10").unwrap(),
            ParsedSelector::Lines {
                ranges: vec![LineRange {
                    start: 50,
                    end: Some(59)
                }],
                raw: false
            }
        );
        // .. 宽容别名
        assert_eq!(
            parse_selector("50..100").unwrap(),
            ParsedSelector::Lines {
                ranges: vec![LineRange {
                    start: 50,
                    end: Some(100)
                }],
                raw: false
            }
        );
        // 多段排序
        assert_eq!(
            parse_selector("960-973,5-16").unwrap(),
            ParsedSelector::Lines {
                ranges: vec![
                    LineRange {
                        start: 5,
                        end: Some(16)
                    },
                    LineRange {
                        start: 960,
                        end: Some(973)
                    }
                ],
                raw: false
            }
        );
    }

    #[test]
    fn parses_compound_raw_range() {
        for input in ["raw:50-100", "50-100:raw"] {
            assert_eq!(
                parse_selector(input).unwrap(),
                ParsedSelector::Lines {
                    ranges: vec![LineRange {
                        start: 50,
                        end: Some(100)
                    }],
                    raw: true
                },
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_malformed() {
        // 与 oh-my-pi 对齐的拒绝集：畸形即报错而非放宽
        for input in [
            "0",             // 行号 1-indexed
            "5+0",           // 计数 >= 1
            "10-5",          // 结束 >= 起点
            ":-10",          // 单独负数
            "1-1:1-2",       // 双 range 复合
            "conflicts:1-1", // conflicts 不可复合
            "raw:conflicts", // 双 raw 类复合
            "abc",
        ] {
            assert!(parse_selector(input).is_err(), "{input} should be rejected");
        }
    }

    #[test]
    fn selector_chain_guard_for_opaque_uris() {
        for input in [
            "12",
            "1-20",
            "30+5",
            "5-16,960-973",
            "raw",
            "conflicts",
            "raw:2-4",
            "-3",
        ] {
            assert!(
                is_selector_chain(input),
                "{input} should be a selector chain"
            );
        }
        for input in ["example:document", "item", "", "1-", "a-b", "1..2"] {
            assert!(
                !is_selector_chain(input),
                "{input} should not be a selector chain"
            );
        }
    }

    #[test]
    fn offset_limit_conversion() {
        let selector = parse_selector("50-100").unwrap();
        assert_eq!(selector.to_offset_limit(), Some((Some(50), Some(51))));
        let selector = parse_selector("50-").unwrap();
        assert_eq!(selector.to_offset_limit(), Some((Some(50), None)));
        // 多段不转
        let selector = parse_selector("1-5,9-10").unwrap();
        assert_eq!(selector.to_offset_limit(), None);
        assert!(parse_selector("raw").unwrap().is_raw());
        assert!(!parse_selector("50-100").unwrap().is_raw());
        assert!(parse_selector("50-100:raw").unwrap().is_raw());
    }
}
