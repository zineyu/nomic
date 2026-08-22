use crate::{PromptTemplate, PromptsError};

/// 将参数串切分为位置参数（shell 风格的引号与转义）。
///
/// 单引号内为字面文本；双引号内 `\"` / `\\` 转义；引号外 `\` 转义下一字符。
/// 引号未闭合时报错。
pub fn split_arguments(input: &str) -> Result<Vec<String>, PromptsError> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_arg = false;
    let mut chars = input.chars();
    let unterminated = || PromptsError::UnterminatedQuote {
        input: input.to_string(),
    };
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            '\'' => {
                in_arg = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(unterminated()),
                    }
                }
            }
            '"' => {
                in_arg = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err(unterminated()),
                        },
                        Some(c) => current.push(c),
                        None => return Err(unterminated()),
                    }
                }
            }
            '\\' => {
                in_arg = true;
                match chars.next() {
                    Some(escaped) => current.push(escaped),
                    None => current.push('\\'),
                }
            }
            c => {
                in_arg = true;
                current.push(c);
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    Ok(args)
}

/// 解析 `/name args...` 形式的模板调用并展开。
///
/// 输入不以 `/` 开头时返回 `Ok(None)`（按普通 prompt 处理）；名称未命中任何
/// 模板时返回 [`PromptsError::NotFound`]；参数串引号未闭合时返回
/// [`PromptsError::UnterminatedQuote`]。内建命令的优先级由调用方保证
/// （先匹配内建命令，未命中再调用本函数）。
pub fn expand_invocation(
    templates: &[PromptTemplate],
    input: &str,
) -> Result<Option<String>, PromptsError> {
    let Some(rest) = input.trim().strip_prefix('/') else {
        return Ok(None);
    };
    let (name, tail) = split_invocation(rest);
    tracing::debug!(name = %name, args = %tail, "expanding prompt template invocation");
    let Some(template) = templates.iter().find(|template| template.name == name) else {
        tracing::debug!(name = %name, available = templates.len(), "prompt template not found");
        return Err(PromptsError::NotFound {
            name: name.to_string(),
            available: templates
                .iter()
                .map(|template| template.name.clone())
                .collect(),
        });
    };
    let args = split_arguments(tail)?;
    tracing::debug!(name = %name, args_count = args.len(), "prompt template expanded");
    Ok(Some(template.expand(&args)))
}

/// 切分 `/name args` 调用：名称到首个空白或冒号为止，其余为参数串。
fn split_invocation(rest: &str) -> (&str, &str) {
    match rest.find(|c: char| c.is_whitespace() || c == ':') {
        Some(pos) => (&rest[..pos], rest[pos + 1..].trim()),
        None => (rest, ""),
    }
}

/// 展开模板正文中的参数占位符。
///
/// 无法识别的 `$` 序列（如 `$0`、`$x`、非法的 `${...}`）保持字面量不变。
pub fn expand_template(body: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos + 1..];
        match expand_one(rest, args) {
            Some((replacement, consumed)) => {
                out.push_str(&replacement);
                rest = &rest[consumed..];
            }
            None => out.push('$'),
        }
    }
    out.push_str(rest);
    out
}

/// 尝试在 `$` 之后解析一个占位符；返回替换文本与消耗的输入长度。
fn expand_one(rest: &str, args: &[String]) -> Option<(String, usize)> {
    if let Some(after) = rest.strip_prefix('{') {
        let end = after.find('}')?;
        let replacement = expand_braced(&after[..end], args)?;
        return Some((replacement, end + 2));
    }
    if let Some(after) = rest.strip_prefix("ARGUMENTS") {
        let _ = after;
        return Some((args.join(" "), "ARGUMENTS".len()));
    }
    if rest.starts_with('@') {
        return Some((args.join(" "), 1));
    }
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 {
        let index: usize = rest[..digits].parse().ok()?;
        // $0 不是合法位置参数，保持字面量
        if index == 0 {
            return None;
        }
        return Some((args.get(index - 1).cloned().unwrap_or_default(), digits));
    }
    None
}

/// 展开 `${...}` 占位符；`content` 为花括号内的文本。
fn expand_braced(content: &str, args: &[String]) -> Option<String> {
    // ${@...} / ${ARGUMENTS...}
    if let Some(rest) = content
        .strip_prefix('@')
        .or_else(|| content.strip_prefix("ARGUMENTS"))
    {
        return expand_all(rest, args);
    }
    // ${N} / ${N:-default}
    let digits = content.len()
        - content
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    if digits == 0 {
        return None;
    }
    let index: usize = content[..digits].parse().ok()?;
    if index == 0 {
        return None;
    }
    let rest = &content[digits..];
    if let Some(default) = rest.strip_prefix(":-") {
        let value = args.get(index - 1).filter(|value| !value.is_empty());
        return Some(value.cloned().unwrap_or_else(|| default.to_string()));
    }
    if rest.is_empty() {
        return Some(args.get(index - 1).cloned().unwrap_or_default());
    }
    None
}

/// 展开 `${@...}` 中 `@` / `ARGUMENTS` 之后的部分：空、`:-default`、`:N`、`:N:L`。
fn expand_all(rest: &str, args: &[String]) -> Option<String> {
    if rest.is_empty() {
        return Some(args.join(" "));
    }
    let spec = rest.strip_prefix(':')?;
    if let Some(default) = spec.strip_prefix('-') {
        let joined = args.join(" ");
        return Some(if joined.is_empty() {
            default.to_string()
        } else {
            joined
        });
    }
    let digits = spec.len() - spec.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let start: usize = spec[..digits].parse().ok()?;
    if start == 0 {
        return None;
    }
    let from = start - 1;
    let tail = &spec[digits..];
    if tail.is_empty() {
        return Some(args.get(from..).unwrap_or(&[]).join(" "));
    }
    let len: usize = tail.strip_prefix(':')?.parse().ok()?;
    let end = from.saturating_add(len).min(args.len());
    Some(args.get(from..end).unwrap_or(&[]).join(" "))
}
