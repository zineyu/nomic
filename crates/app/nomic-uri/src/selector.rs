//! URI 尾挂选择器（T3 扩展完整解析）。
//!
//! 本模块当前只提供 [`is_selector_chain`]：opaque URI 防误判的共享判定
//!（`Makefile:12` / `notes:raw` 不是 URI）。

/// 选择器链判定：一个以上选择器块以 `:` 相连。
///
/// 块语法（对齐 oh-my-pi `parse.ts` 的 `SELECTOR_CHUNK_SRC`，大小写不敏感）：
/// `raw | conflicts | -?\d+([-+]\d+)?(,\d+([-+]\d+)?)*`
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
    let mut parts = chunk.split(',');
    let Some(first) = parts.next() else {
        return false;
    };
    is_range_part(first, true) && parts.all(|part| is_range_part(part, false))
}

/// `\d+([-+]\d+)?`；首段额外允许前导 `-`。
fn is_range_part(part: &str, allow_leading_minus: bool) -> bool {
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

#[cfg(test)]
mod tests {
    use super::is_selector_chain;

    #[test]
    fn recognizes_selector_chains() {
        for input in [
            "12",
            "1-20",
            "30+5",
            "5-16,960-973",
            "raw",
            "RAW",
            "conflicts",
            "raw:2-4",
            "1-20:raw",
            "-3",
        ] {
            assert!(is_selector_chain(input), "{input} should be a selector chain");
        }
    }

    #[test]
    fn rejects_non_selectors() {
        for input in ["example:document", "item", "", "1-", "1-", "a-b", "1..2"] {
            assert!(!is_selector_chain(input), "{input} should not be a selector chain");
        }
    }
}
