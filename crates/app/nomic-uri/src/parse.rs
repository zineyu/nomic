//! 容错内部 URI 解析器。
//!
//! 标准 URL 解析会把 `skill://plugin:name` 里的冒号当端口分隔符，因此这里
//! 正则式地手工提取 scheme / host / path / query / fragment，并额外保留
//! 原始形态字段：
//!
//! - [`InternalUri::raw_host`]：percent 解码后、保留大小写的 authority；
//! - [`InternalUri::raw_path`]：归一化**之前**的 path（保留 `..` 等穿越
//!   标记——containment 校验必须基于原始形态）；
//! - [`InternalUri::raw_href`]：字节级原样的输入串。
//!
//! 所有解析内部 URI 的代码必须用 [`parse_internal_uri`]，不得自行切分字符串。

use crate::handler::UriError;

/// 解析后的内部 URI。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternalUri {
    /// 小写 scheme（不含 `://`）
    pub scheme: String,
    /// percent 解码后的 authority，保留大小写
    pub raw_host: String,
    /// 归一化之前的 path（不含前导 `/` 之外的处理；空串表示无 path）
    pub raw_path: String,
    /// 查询参数对（percent 解码后）
    pub query: Vec<(String, String)>,
    /// 片段（percent 解码后）
    pub fragment: Option<String>,
    /// 字节级原样的输入
    pub raw_href: String,
}

impl InternalUri {
    /// 取查询参数（同名取第一个）。
    #[must_use]
    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// scheme 首字符：`[a-z]`（大小写不敏感）。
const fn is_scheme_first(ch: u8) -> bool {
    ch.is_ascii_alphabetic()
}

/// scheme 后续字符：`[a-z0-9+.-]`。
const fn is_scheme_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, b'+' | b'-' | b'.')
}

/// 从开头匹配 scheme，返回 (scheme, scheme 字节长度)。
fn match_scheme(input: &str) -> Option<(&str, usize)> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || !is_scheme_first(bytes[0]) {
        return None;
    }
    let mut end = 1;
    while end < bytes.len() && is_scheme_char(bytes[end]) {
        end += 1;
    }
    Some((&input[..end], end))
}

/// 提取 URI 形输入的小写 scheme；不像 URI 时返回 `None`。
///
/// 接受层级形式（`scheme://…`）与 opaque 形式（`scheme:rest`）。opaque 形式
/// 有三重防误判：
/// - 单字母 scheme（Windows 盘符 `C:\…`、`C:/…`、`C:foo`）；
/// - scheme 含 `.`（文件名带扩展名，如 `foo.ts:50`——其实 scheme 语法允许
///   `.`，但 `foo.ts` 几乎不可能是真 scheme）；
/// - 尾巴匹配 read 选择器链（`Makefile:12`、`README:raw:1-20`）。
#[must_use]
pub fn extract_uri_scheme(input: &str) -> Option<String> {
    let (scheme, scheme_len) = match_scheme(input)?;
    let rest = &input[scheme_len..];
    if let Some(after) = rest.strip_prefix("://") {
        // 层级形式：scheme 即答案（host 可为空，如 `omp://`）。
        let _ = after;
        return Some(scheme.to_ascii_lowercase());
    }
    // opaque 形式：`scheme:rest`，rest 非空。
    let rest = rest.strip_prefix(':')?;
    if rest.is_empty() {
        return None;
    }
    if scheme.len() == 1 || scheme.contains('.') {
        return None;
    }
    if crate::selector::is_selector_chain(rest) {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

/// 提取层级形式（`scheme://…`）的小写 scheme；非层级形式返回 `None`。
///
/// 与 [`extract_uri_scheme`] 的区别：不接受 opaque 形式，也无防误判守卫——
/// 调用方（router）只关心严格的 `scheme://` 前缀。
#[must_use]
pub fn hierarchical_scheme(input: &str) -> Option<String> {
    let (scheme, scheme_len) = match_scheme(input)?;
    input[scheme_len..]
        .strip_prefix("://")
        .map(|_| scheme.to_ascii_lowercase())
}

/// percent 解码；非法序列按原样保留（lossy，不报错）。
#[must_use]
pub fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).copied().and_then(hex_val);
            let lo = bytes.get(i + 2).copied().and_then(hex_val);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const fn hex_val(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(ch - b'a' + 10),
        b'A'..=b'F' => Some(ch - b'A' + 10),
        _ => None,
    }
}

/// 解析 `scheme://host/path?query#fragment` 形式的内部 URI。
///
/// host 段允许冒号（`skill://plugin:name` 的冒号不是端口分隔符）。
/// 非层级形式（缺 `://`）报错——opaque URI 不由本函数解析。
pub fn parse_internal_uri(input: &str) -> Result<InternalUri, UriError> {
    let (scheme, scheme_len) =
        match_scheme(input).ok_or_else(|| UriError::InvalidUri(input.to_string()))?;
    let rest = &input[scheme_len..];
    let rest = rest
        .strip_prefix("://")
        .ok_or_else(|| UriError::InvalidUri(input.to_string()))?;

    // fragment 最右优先：`#` 之后全部属于 fragment。
    let (before_fragment, fragment) = match rest.find('#') {
        Some(idx) => (&rest[..idx], Some(percent_decode(&rest[idx + 1..]))),
        None => (rest, None),
    };
    // query：`?` 之后、fragment 之前。
    let (before_query, query_raw) = match before_fragment.find('?') {
        Some(idx) => (&before_fragment[..idx], Some(&before_fragment[idx + 1..])),
        None => (before_fragment, None),
    };
    // authority：到首个 `/` 为止。
    let (authority, path_raw) = match before_query.find('/') {
        Some(idx) => (&before_query[..idx], &before_query[idx..]),
        None => (before_query, ""),
    };

    let query = query_raw.map_or_else(Vec::new, |raw| {
        raw.split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) => (percent_decode(key), percent_decode(value)),
                None => (percent_decode(pair), String::new()),
            })
            .collect()
    });

    Ok(InternalUri {
        scheme: scheme.to_ascii_lowercase(),
        raw_host: percent_decode(authority),
        // 去掉前导 `/`，保留其余原样（含 `..`、尾部 `/`）。
        raw_path: path_raw.strip_prefix('/').unwrap_or(path_raw).to_string(),
        query,
        fragment,
        raw_href: input.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_hierarchical_scheme() {
        assert_eq!(extract_uri_scheme("skill://pdf"), Some("skill".into()));
        assert_eq!(extract_uri_scheme("Artifact://3"), Some("artifact".into()));
        assert_eq!(extract_uri_scheme("omp://"), Some("omp".into()));
    }

    #[test]
    fn extracts_opaque_scheme_with_guards() {
        assert_eq!(extract_uri_scheme("urn:example:doc"), Some("urn".into()));
        // Windows 盘符
        assert_eq!(extract_uri_scheme("C:\\foo"), None);
        assert_eq!(extract_uri_scheme("C:/foo"), None);
        assert_eq!(extract_uri_scheme("C:foo"), None);
        // 文件名带扩展名
        assert_eq!(extract_uri_scheme("foo.ts:50"), None);
        // read 选择器尾巴
        assert_eq!(extract_uri_scheme("Makefile:12"), None);
        assert_eq!(extract_uri_scheme("README:raw:1-20"), None);
        assert_eq!(extract_uri_scheme("notes:conflicts"), None);
        // 普通相对/绝对路径
        assert_eq!(extract_uri_scheme("src/main.rs"), None);
        assert_eq!(extract_uri_scheme("/abs/path"), None);
        assert_eq!(extract_uri_scheme("no-scheme"), None);
    }

    #[test]
    fn parses_host_with_colon() {
        let uri = parse_internal_uri("skill://plugin:name/scripts/run.sh").expect("parse");
        assert_eq!(uri.scheme, "skill");
        assert_eq!(uri.raw_host, "plugin:name");
        assert_eq!(uri.raw_path, "scripts/run.sh");
    }

    #[test]
    fn preserves_raw_forms() {
        let uri = parse_internal_uri("Local://Plan.md").expect("parse");
        assert_eq!(uri.scheme, "local");
        assert_eq!(uri.raw_host, "Plan.md"); // 大小写保留

        let uri = parse_internal_uri("memory://root/../escape").expect("parse");
        assert_eq!(uri.raw_path, "../escape"); // 穿越标记保留

        let input = "artifact://3?x=1#frag";
        let uri = parse_internal_uri(input).expect("parse");
        assert_eq!(uri.raw_href, input);
        assert_eq!(uri.query_param("x"), Some("1"));
        assert_eq!(uri.fragment.as_deref(), Some("frag"));
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("alice%40prod"), "alice@prod");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("100%"), "100%"); // 非法序列原样保留
        assert_eq!(percent_decode("%zz"), "%zz");
        let uri = parse_internal_uri("ssh://alice%40prod/etc/hosts").expect("parse");
        assert_eq!(uri.raw_host, "alice@prod");
    }

    #[test]
    fn rejects_non_hierarchical_input() {
        assert!(parse_internal_uri("urn:example:doc").is_err());
        assert!(parse_internal_uri("plain/path").is_err());
        assert!(parse_internal_uri("skill:/one-slash").is_err());
    }

    #[test]
    fn empty_host_and_path() {
        let uri = parse_internal_uri("omp://").expect("parse");
        assert_eq!(uri.raw_host, "");
        assert_eq!(uri.raw_path, "");
        let uri = parse_internal_uri("history://").expect("parse");
        assert_eq!(uri.raw_host, "");
    }
}
