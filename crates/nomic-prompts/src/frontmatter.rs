/// 分离 frontmatter；返回 `(frontmatter, body)`。
pub fn split_frontmatter(text: &str) -> Result<(Option<&str>, &str), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !text.starts_with("---\n") && !text.starts_with("---\r\n") {
        return Ok((None, text));
    }
    let after_start = if text.starts_with("---\r\n") { 5 } else { 4 };
    let rest = &text[after_start..];
    for (index, _) in rest.match_indices("---") {
        let line_start = index == 0 || rest[..index].ends_with('\n');
        if !line_start {
            continue;
        }
        let line_end = rest[index..]
            .find('\n')
            .map_or(rest.len(), |offset| index + offset);
        let marker = rest[index..line_end].trim_end_matches('\r');
        if marker == "---" {
            return Ok((Some(&rest[..index]), &rest[line_end..]));
        }
    }
    Err("frontmatter starts with '---' but has no closing '---' line".to_string())
}

#[derive(Debug, Default)]
pub struct Frontmatter {
    pub description: Option<String>,
    pub argument_hint: Option<String>,
}

/// 解析受支持的 frontmatter 子集。
pub fn parse_frontmatter(text: &str) -> Result<Frontmatter, String> {
    let mut result = Frontmatter::default();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            return Err(format!("unsupported frontmatter line {trimmed:?}"));
        };
        let key = key.trim();
        let value = value.trim();
        // 块标量（`>` 折叠 / `|` 字面，可带 `+`/`-` chomping 标记）：
        // 消费后续缩进续行，折叠形式以空格连接，字面形式以换行连接。
        let block =
            block_scalar_indicator(value).map(|folded| read_block_scalar(&mut lines, folded));
        let value = block.as_deref().unwrap_or(value);
        match key {
            "description" => result.description = Some(unquote(value)),
            "argument-hint" => result.argument_hint = Some(unquote(value)),
            _ => {
                if block.is_some() {
                    // 未知键的块标量已消费完毕，直接忽略。
                } else if value.is_empty() {
                    // 未知键的嵌套块（map / 列表）：跳过所有缩进续行。
                    skip_nested_block(&mut lines);
                } else if value.starts_with(['[', '{']) {
                    return Err(format!(
                        "unsupported frontmatter field {key:?}; only scalar unknown fields are ignored"
                    ));
                }
            }
        }
    }
    Ok(result)
}

/// 判断 value 是否为 YAML 块标量指示符；返回 `Some(true)` 表示折叠式（`>`）。
pub fn block_scalar_indicator(value: &str) -> Option<bool> {
    match value {
        ">" | ">-" | ">+" => Some(true),
        "|" | "|-" | "|+" => Some(false),
        _ => None,
    }
}

/// 读取块标量的缩进续行（空行终止，属于受支持的最小子集）。
pub fn read_block_scalar(
    lines: &mut std::iter::Peekable<std::str::Lines<'_>>,
    folded: bool,
) -> String {
    let mut parts = Vec::new();
    while let Some(next) = lines.peek() {
        if next.trim().is_empty() || !next.starts_with([' ', '\t']) {
            break;
        }
        parts.push(next.trim());
        lines.next();
    }
    if folded {
        parts.join(" ")
    } else {
        parts.join("\n")
    }
}

/// 跳过未知键下的嵌套块（所有缩进续行及其间空行）。
pub fn skip_nested_block(lines: &mut std::iter::Peekable<std::str::Lines<'_>>) {
    while let Some(next) = lines.peek() {
        if next.trim().is_empty() || next.starts_with([' ', '\t']) {
            lines.next();
        } else {
            break;
        }
    }
}

/// 去掉简单的单双引号。
pub fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
            || (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"'))
    {
        return value[1..value.len() - 1].to_string();
    }
    value.to_string()
}

/// 缺省描述：正文第一个非空行，去掉 Markdown heading 符号。
pub fn fallback_description(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(
            || "No description".to_string(),
            |line| line.trim_start_matches('#').trim().to_string(),
        )
}
