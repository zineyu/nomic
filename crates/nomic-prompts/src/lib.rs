//! nomic-prompts：prompt template 发现、元数据解析与参数展开。
//!
//! prompt template 是一个 `.md` 文件：文件名（去掉 `.md`）即命令名，正文是模板。
//! 在编辑器中输入 `/name 参数...`，模板展开为完整 prompt 后提交。
//!
//! 支持三类来源（低优先级 → 高优先级，同名覆盖）：
//!
//! - 用户级：`~/.config/nomic/prompts/*.md` 与 `$XDG_CONFIG_HOME/nomic/prompts/*.md`
//! - 项目级：当前目录向上发现的 `.nomic/prompts/*.md`（越靠近 cwd 越优先）
//! - 显式：配置文件 `prompts` 数组与 `--prompt-template` 指定的文件或目录
//!
//! 目录发现是非递归的；模板名规则与 skill 名一致（1～64 个小写 ASCII 字母、
//! 数字、`-`、`_`，且不能以 `-`/`_` 开头或结尾）。
//!
//! 模板正文的参数占位符（与 pi 对齐）：
//!
//! - `$1`、`$2`、...：位置参数（缺失时展开为空）
//! - `$@` / `$ARGUMENTS`：全部参数（空格连接）
//! - `${1:-default}` / `${@:-default}` / `${ARGUMENTS:-default}`：缺失或为空时用默认值
//! - `${@:N}`：第 N 个（1 起）及之后的全部参数
//! - `${@:N:L}`：从第 N 个起取 L 个参数

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

/// 已发现的 prompt template。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    /// 模板名（文件名去掉 `.md`，即 `/name` 中的 name）
    pub name: String,
    /// 模板文件的真实路径
    pub path: PathBuf,
    /// 模板来源
    pub scope: PromptScope,
    /// 简短描述（frontmatter `description`，缺省退化为正文第一个非空行）
    pub description: String,
    /// 参数提示（frontmatter `argument-hint`，补全弹层展示用）
    pub argument_hint: Option<String>,
    /// 去掉 frontmatter 后的模板正文
    pub body: String,
}

impl PromptTemplate {
    /// 以给定参数展开模板正文。
    pub fn expand(&self, args: &[String]) -> String {
        expand_template(&self.body, args)
    }
}

/// 模板来源层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PromptScope {
    /// 用户配置目录
    User,
    /// 项目目录（含向上继承的项目目录）
    Project,
    /// 显式指定的文件或目录（配置文件 `prompts` / `--prompt-template`）
    Explicit,
}

impl PromptScope {
    /// 序列化到标签 / 提示文本的稳定文本形式。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Explicit => "explicit",
        }
    }
}

impl fmt::Display for PromptScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 项目级目录发现规则（预留显式 roots 模式，便于测试和自定义集成）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectDiscovery {
    /// 从当前 cwd 向上查找到文件系统根
    Ancestors,
    /// 只使用显式提供的一组项目根
    Roots(Vec<PathBuf>),
}

/// 一个模板根目录（其中每个 `.md` 文件是一个模板）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptRoot {
    /// 根目录路径
    pub path: PathBuf,
    /// 来源层级
    pub scope: PromptScope,
}

/// 模板发现与解析入口。
#[derive(Debug, Clone)]
pub struct PromptResolver {
    roots: Vec<PromptRoot>,
    /// 显式指定的模板文件或目录（最高优先级）
    explicit: Vec<PathBuf>,
}

impl PromptResolver {
    /// 按当前 cwd 构造标准 resolver。
    ///
    /// 项目目录从 `cwd` 向上查找 `.nomic/prompts`；用户目录按 XDG / HOME 解析。
    /// `HOME` 缺失时仍允许只使用 XDG 用户目录。
    pub fn for_cwd(cwd: &Path) -> Result<Self, PromptsError> {
        Self::new(
            cwd,
            ProjectDiscovery::Ancestors,
            default_user_roots(),
            Vec::new(),
        )
    }

    /// 显式构造 resolver（测试与自定义目录）。
    ///
    /// `ProjectDiscovery::Roots` 中的根应按低优先级到高优先级传入；
    /// `ProjectDiscovery::Ancestors` 自动保证越靠近 cwd 优先级越高。
    pub fn new(
        cwd: &Path,
        project_discovery: ProjectDiscovery,
        user_roots: Vec<PromptRoot>,
        explicit: Vec<PathBuf>,
    ) -> Result<Self, PromptsError> {
        if !cwd.is_absolute() {
            return Err(PromptsError::RelativeCwd(cwd.to_path_buf()));
        }
        let project_roots = discover_project_roots(cwd, project_discovery);
        // roots 按低优先级到高优先级排列（catalog 中后写入者覆盖先写入者）：
        // 用户级在前，项目级在后，显式路径经 self.explicit 最后处理。
        let mut roots = user_roots;
        roots.extend(project_roots);
        Ok(Self { roots, explicit })
    }

    /// 追加显式模板文件或目录（配置文件 `prompts` / `--prompt-template`）。
    #[must_use]
    pub fn with_explicit(mut self, paths: Vec<PathBuf>) -> Self {
        self.explicit = paths;
        self
    }

    /// 发现并按覆盖规则返回全部可用模板。
    ///
    /// 加载失败的单个模板（名称非法、文件不可读、frontmatter 非法）会被跳过，
    /// 不影响其他模板；需要诊断信息时使用 [`Self::catalog_with_diagnostics`]。
    pub fn catalog(&self) -> Vec<PromptTemplate> {
        self.catalog_with_diagnostics().templates
    }

    /// 同 [`Self::catalog`]，同时返回被跳过模板的诊断信息。
    pub fn catalog_with_diagnostics(&self) -> PromptCatalog {
        let mut by_name: BTreeMap<String, PromptTemplate> = BTreeMap::new();
        let mut errors = Vec::new();
        for root in &self.roots {
            scan_dir(&root.path, root.scope, &mut by_name, &mut errors);
        }
        for path in &self.explicit {
            if path.is_dir() {
                scan_dir(path, PromptScope::Explicit, &mut by_name, &mut errors);
            } else {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match validate_template_name(&name)
                    .and_then(|()| load_template(path, name.clone(), PromptScope::Explicit))
                {
                    Ok(template) => {
                        by_name.insert(name, template);
                    }
                    Err(error) => errors.push(error),
                }
            }
        }
        PromptCatalog {
            templates: by_name.into_values().collect(),
            errors,
        }
    }

    /// 按名称解析一个模板。
    pub fn resolve(&self, name: &str) -> Result<PromptTemplate, PromptsError> {
        validate_template_name(name)?;
        self.catalog()
            .into_iter()
            .find(|template| template.name == name)
            .ok_or_else(|| PromptsError::NotFound {
                name: name.to_string(),
                available: self.available_names(),
            })
    }

    /// 按名称解析并展开模板。
    pub fn expand(&self, name: &str, args: &[String]) -> Result<String, PromptsError> {
        Ok(self.resolve(name)?.expand(args))
    }

    fn available_names(&self) -> Vec<String> {
        self.catalog()
            .into_iter()
            .map(|template| template.name)
            .collect()
    }
}

/// [`PromptResolver::catalog_with_diagnostics`] 的结果。
#[derive(Debug)]
pub struct PromptCatalog {
    /// 成功加载的模板（已按覆盖规则去重）
    pub templates: Vec<PromptTemplate>,
    /// 加载失败被跳过的模板及原因
    pub errors: Vec<PromptsError>,
}

/// prompts 系统错误。
#[derive(Debug, thiserror::Error)]
pub enum PromptsError {
    /// cwd 必须是绝对路径
    #[error("prompt resolver requires an absolute current directory, got {}", .0.display())]
    RelativeCwd(PathBuf),
    /// 模板名非法
    #[error(
        "invalid prompt template name {name:?}; use 1-64 chars of lowercase ASCII letters, digits, '-' or '_' (cannot start/end with '-' or '_')"
    )]
    InvalidName {
        /// 非法名称
        name: String,
    },
    /// 找不到指定模板
    #[error("prompt template {name:?} not found{}", if available.is_empty() { String::new() } else { format!("; available: {}", available.join(", ")) })]
    NotFound {
        /// 请求的名称
        name: String,
        /// 当前可用名称
        available: Vec<String>,
    },
    /// 读取模板文件失败
    #[error("failed to read prompt template {}: {message}", .path.display())]
    ReadTemplateFile {
        /// 文件路径
        path: PathBuf,
        /// 底层错误
        message: String,
    },
    /// frontmatter 非法
    #[error("invalid prompt template frontmatter in {}: {message}", .path.display())]
    InvalidFrontmatter {
        /// 文件路径
        path: PathBuf,
        /// 错误说明
        message: String,
    },
    /// 目录扫描失败
    #[error("failed to scan prompts directory {}: {message}", .path.display())]
    ReadDir {
        /// 目录路径
        path: PathBuf,
        /// 底层错误
        message: String,
    },
    /// 参数串存在未闭合的引号
    #[error("unterminated quote in template arguments: {input:?}")]
    UnterminatedQuote {
        /// 原始参数串
        input: String,
    },
}

/// 默认用户级模板根（低优先级在前，高优先级在后）。
fn default_user_roots() -> Vec<PromptRoot> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        roots.push(PromptRoot {
            path: PathBuf::from(&home)
                .join(".config")
                .join("nomic")
                .join("prompts"),
            scope: PromptScope::User,
        });
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        roots.push(PromptRoot {
            path: PathBuf::from(xdg).join("nomic").join("prompts"),
            scope: PromptScope::User,
        });
    }
    roots
}

/// 发现项目级模板根。返回顺序从低优先级到高优先级。
fn discover_project_roots(cwd: &Path, discovery: ProjectDiscovery) -> Vec<PromptRoot> {
    let roots = match discovery {
        ProjectDiscovery::Ancestors => {
            let mut ancestors = cwd.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
            ancestors.reverse();
            ancestors
        }
        ProjectDiscovery::Roots(roots) => roots,
    };
    roots
        .into_iter()
        .map(|root| PromptRoot {
            path: root.join(".nomic").join("prompts"),
            scope: PromptScope::Project,
        })
        .collect()
}

/// 扫描一个模板目录（非递归，只看 `*.md` 文件），按覆盖规则写入 `by_name`。
fn scan_dir(
    dir: &Path,
    scope: PromptScope,
    by_name: &mut BTreeMap<String, PromptTemplate>,
    errors: &mut Vec<PromptsError>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(PromptsError::ReadDir {
                    path: dir.to_path_buf(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        let path = entry.path();
        if !path.is_file() || path.extension() != Some(OsStr::new("md")) {
            continue;
        }
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        match validate_template_name(&name).and_then(|()| load_template(&path, name.clone(), scope))
        {
            Ok(template) => {
                by_name.insert(name, template);
            }
            Err(error) => errors.push(error),
        }
    }
}

/// 校验模板名，避免路径穿越与调用歧义（规则与 skill 名一致）。
fn validate_template_name(name: &str) -> Result<(), PromptsError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !name.starts_with(['-', '_'])
        && !name.ends_with(['-', '_']);
    if valid {
        Ok(())
    } else {
        Err(PromptsError::InvalidName {
            name: name.to_string(),
        })
    }
}

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
    let Some(template) = templates.iter().find(|template| template.name == name) else {
        return Err(PromptsError::NotFound {
            name: name.to_string(),
            available: templates
                .iter()
                .map(|template| template.name.clone())
                .collect(),
        });
    };
    let args = split_arguments(tail)?;
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

/// 加载模板文件并解析可选 YAML frontmatter 的最小兼容子集。
///
/// 与 skill frontmatter 同一口径（避免为文档引入完整 YAML 依赖）：
/// - `description: text`，或块标量形式 `>-` / `|` 加缩进续行
/// - `argument-hint: text`（简单标量）
/// - 其他简单标量键被忽略；未知键的嵌套块被跳过；其余复杂 YAML 明确报错
fn load_template(
    path: &Path,
    name: String,
    scope: PromptScope,
) -> Result<PromptTemplate, PromptsError> {
    let text = std::fs::read_to_string(path).map_err(|error| PromptsError::ReadTemplateFile {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let (frontmatter, body) =
        split_frontmatter(&text).map_err(|message| PromptsError::InvalidFrontmatter {
            path: path.to_path_buf(),
            message,
        })?;
    let parsed = if let Some(frontmatter) = frontmatter {
        parse_frontmatter(frontmatter).map_err(|message| PromptsError::InvalidFrontmatter {
            path: path.to_path_buf(),
            message,
        })?
    } else {
        Frontmatter::default()
    };
    let body = body.trim().to_string();
    let description = parsed
        .description
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback_description(&body));
    let argument_hint = parsed
        .argument_hint
        .filter(|value| !value.trim().is_empty());
    Ok(PromptTemplate {
        name,
        path: path.to_path_buf(),
        scope,
        description,
        argument_hint,
        body,
    })
}

/// 分离 frontmatter；返回 `(frontmatter, body)`。
fn split_frontmatter(text: &str) -> Result<(Option<&str>, &str), String> {
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
struct Frontmatter {
    description: Option<String>,
    argument_hint: Option<String>,
}

/// 解析受支持的 frontmatter 子集。
fn parse_frontmatter(text: &str) -> Result<Frontmatter, String> {
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
fn block_scalar_indicator(value: &str) -> Option<bool> {
    match value {
        ">" | ">-" | ">+" => Some(true),
        "|" | "|-" | "|+" => Some(false),
        _ => None,
    }
}

/// 读取块标量的缩进续行（空行终止，属于受支持的最小子集）。
fn read_block_scalar(lines: &mut std::iter::Peekable<std::str::Lines<'_>>, folded: bool) -> String {
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
fn skip_nested_block(lines: &mut std::iter::Peekable<std::str::Lines<'_>>) {
    while let Some(next) = lines.peek() {
        if next.trim().is_empty() || next.starts_with([' ', '\t']) {
            lines.next();
        } else {
            break;
        }
    }
}

/// 去掉简单的单双引号。
fn unquote(value: &str) -> String {
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
fn fallback_description(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(
            || "No description".to_string(),
            |line| line.trim_start_matches('#').trim().to_string(),
        )
}

#[cfg(test)]
// 测试数据大量包含模板占位符字面量（${1:-default} 等），并非格式化参数
#[allow(clippy::literal_string_with_formatting_args)]
mod tests {
    use super::*;

    fn temp_template(root: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(root).expect("mkdir");
        std::fs::write(root.join(format!("{name}.md")), text).expect("write");
    }

    fn resolver(cwd: &Path, roots: Vec<(&Path, PromptScope)>) -> PromptResolver {
        PromptResolver::new(
            cwd,
            ProjectDiscovery::Roots(Vec::new()),
            roots
                .into_iter()
                .map(|(path, scope)| PromptRoot {
                    path: path.to_path_buf(),
                    scope,
                })
                .collect(),
            Vec::new(),
        )
        .expect("resolver")
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_template(
            tmp.path(),
            "review",
            "---\ndescription: Review staged git changes\nargument-hint: \"<path>\"\n---\nReview the staged changes.\n",
        );
        let template = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)])
            .resolve("review")
            .expect("resolve");
        assert_eq!(template.description, "Review staged git changes");
        assert_eq!(template.argument_hint.as_deref(), Some("<path>"));
        assert_eq!(template.body, "Review the staged changes.");
        assert_eq!(template.scope, PromptScope::Project);
    }

    #[test]
    fn fallback_description_uses_first_non_empty_line() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_template(tmp.path(), "plain", "\n# Do useful work\nBody\n");
        let template = resolver(tmp.path(), vec![(tmp.path(), PromptScope::User)])
            .resolve("plain")
            .expect("resolve");
        assert_eq!(template.description, "Do useful work");
        assert_eq!(template.argument_hint, None);
    }

    #[test]
    fn higher_priority_root_overrides_same_name() {
        let tmp = tempfile::tempdir().expect("tmp");
        let low = tmp.path().join("low");
        let high = tmp.path().join("high");
        temp_template(&low, "shared", "low body");
        temp_template(&high, "shared", "high body");
        let template = resolver(
            tmp.path(),
            vec![
                (low.as_path(), PromptScope::User),
                (high.as_path(), PromptScope::Project),
            ],
        )
        .resolve("shared")
        .expect("resolve");
        assert_eq!(template.body, "high body");
        assert_eq!(template.scope, PromptScope::Project);
    }

    #[test]
    fn explicit_path_overrides_discovered_and_accepts_file_or_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        let discovered = tmp.path().join("discovered");
        temp_template(&discovered, "shared", "discovered body");
        temp_template(&discovered, "other", "other body");
        let explicit_dir = tmp.path().join("explicit");
        temp_template(&explicit_dir, "shared", "explicit dir body");
        let explicit_file = tmp.path().join("single.md");
        std::fs::write(&explicit_file, "explicit file body").expect("write");
        let resolver = PromptResolver::new(
            tmp.path(),
            ProjectDiscovery::Roots(Vec::new()),
            vec![PromptRoot {
                path: discovered,
                scope: PromptScope::Project,
            }],
            vec![explicit_dir, explicit_file],
        )
        .expect("resolver");
        assert_eq!(
            resolver.resolve("shared").expect("resolve").body,
            "explicit dir body"
        );
        assert_eq!(
            resolver.resolve("shared").expect("resolve").scope,
            PromptScope::Explicit
        );
        assert_eq!(
            resolver.resolve("single").expect("resolve").body,
            "explicit file body"
        );
        assert_eq!(
            resolver.resolve("other").expect("resolve").body,
            "other body"
        );
    }

    #[test]
    fn project_templates_prefer_nearer_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        temp_template(&outer.join(".nomic/prompts"), "shared", "outer body");
        temp_template(&inner.join(".nomic/prompts"), "shared", "inner body");
        let resolver =
            PromptResolver::new(&inner, ProjectDiscovery::Ancestors, Vec::new(), Vec::new())
                .expect("resolver");
        assert_eq!(
            resolver.resolve("shared").expect("resolve").body,
            "inner body"
        );
    }

    #[test]
    fn discovery_is_non_recursive() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_template(tmp.path(), "top", "top body");
        temp_template(&tmp.path().join("nested"), "nested", "nested body");
        let catalog = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)]).catalog();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "top");
    }

    #[test]
    fn broken_template_is_skipped_without_breaking_catalog() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_template(tmp.path(), "good", "good body");
        temp_template(
            tmp.path(),
            "broken",
            "---\nmetadata: {nested: flow}\n---\nbody\n",
        );
        // 非法名称的文件同样只被跳过。
        temp_template(tmp.path(), "BadName", "bad name body");
        // 非 .md 文件不参与发现。
        std::fs::write(tmp.path().join("notes.txt"), "not a template").expect("write");
        let catalog =
            resolver(tmp.path(), vec![(tmp.path(), PromptScope::User)]).catalog_with_diagnostics();
        assert_eq!(catalog.templates.len(), 1);
        assert_eq!(catalog.templates[0].name, "good");
        assert_eq!(catalog.errors.len(), 2);
        assert!(
            catalog
                .errors
                .iter()
                .any(|error| matches!(error, PromptsError::InvalidFrontmatter { .. }))
        );
        assert!(
            catalog
                .errors
                .iter()
                .any(|error| matches!(error, PromptsError::InvalidName { .. }))
        );
    }

    #[test]
    fn rejects_path_traversal_name() {
        let tmp = tempfile::tempdir().expect("tmp");
        let resolver = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)]);
        let error = resolver.resolve("../secret").expect_err("invalid");
        assert!(matches!(error, PromptsError::InvalidName { .. }));
    }

    #[test]
    fn resolve_not_found_lists_available() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_template(tmp.path(), "review", "body");
        let error = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)])
            .resolve("missing")
            .expect_err("not found");
        let PromptsError::NotFound { available, .. } = error else {
            panic!("expected NotFound");
        };
        assert_eq!(available, vec!["review"]);
    }

    #[test]
    fn parses_block_scalar_description_and_nested_unknown_fields() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_template(
            tmp.path(),
            "folded",
            "---\ndescription: >-\n  First line\n  second line\nmetadata:\n  category: test\n---\nBody\n",
        );
        let template = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)])
            .resolve("folded")
            .expect("resolve");
        assert_eq!(template.description, "First line second line");
        assert_eq!(template.body, "Body");
    }

    // ── 参数切分 ────────────────────────────────────────────────────────────

    #[test]
    fn split_arguments_handles_quotes_and_escapes() {
        assert_eq!(
            split_arguments("Button \"click handler\" 'disabled support'").expect("split"),
            vec!["Button", "click handler", "disabled support"]
        );
        assert_eq!(
            split_arguments("a\\ b \"c\\\"d\" 'e\\f'").expect("split"),
            vec!["a b", "c\"d", "e\\f"]
        );
        assert!(split_arguments("").expect("split").is_empty());
        assert_eq!(
            split_arguments("  \t ").expect("split"),
            Vec::<String>::new()
        );
        // 空引号是一个空参数
        assert_eq!(split_arguments("\"\"").expect("split"), vec![""]);
        let error = split_arguments("\"unterminated").expect_err("unterminated");
        assert!(matches!(error, PromptsError::UnterminatedQuote { .. }));
        assert!(split_arguments("'unterminated").is_err());
    }

    // ── 模板展开 ────────────────────────────────────────────────────────────

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn expands_positional_and_all_arguments() {
        let body = "Create a component named $1 with features: $@";
        assert_eq!(
            expand_template(
                body,
                &args(&["Button", "onClick handler", "disabled support"])
            ),
            "Create a component named Button with features: Button onClick handler disabled support"
        );
        assert_eq!(
            expand_template("args: $ARGUMENTS", &args(&["a", "b"])),
            "args: a b"
        );
        // 缺失的位置参数展开为空
        assert_eq!(expand_template("<$2>", &args(&["a"])), "<>");
        assert_eq!(expand_template("<$10>", &args(&["a"])), "<>");
    }

    #[test]
    fn expands_defaults() {
        assert_eq!(
            expand_template("Summarize in ${1:-7} bullet points.", &args(&[])),
            "Summarize in 7 bullet points."
        );
        assert_eq!(
            expand_template("Summarize in ${1:-7} bullet points.", &args(&["3"])),
            "Summarize in 3 bullet points."
        );
        // 空参数视为缺失
        assert_eq!(expand_template("${1:-fallback}", &args(&[""])), "fallback");
        assert_eq!(expand_template("${@:-nothing}", &args(&[])), "nothing");
        assert_eq!(expand_template("${ARGUMENTS:-nothing}", &args(&["x"])), "x");
    }

    #[test]
    fn expands_argument_slices() {
        let values = args(&["a", "b", "c", "d"]);
        assert_eq!(expand_template("${@:2}", &values), "b c d");
        assert_eq!(expand_template("${@:2:2}", &values), "b c");
        assert_eq!(expand_template("${@:3:5}", &values), "c d");
        // 越界展开为空
        assert_eq!(expand_template("<${@:9}>", &values), "<>");
        assert_eq!(expand_template("<${@:9:2}>", &values), "<>");
    }

    #[test]
    fn unrecognized_dollar_sequences_stay_literal() {
        let values = args(&["a"]);
        // $5 是合法位置参数（缺失展开为空）；$x / $0 保持字面量
        assert_eq!(
            expand_template("cost is $5 and $x and $0", &values),
            "cost is  and $x and $0"
        );
        // 非法 brace 形式保持字面量
        assert_eq!(
            expand_template("${1:2} ${foo} ${@:0}", &values),
            "${1:2} ${foo} ${@:0}"
        );
        // 结尾孤立的 $
        assert_eq!(expand_template("trailing $", &values), "trailing $");
    }

    #[test]
    fn template_expand_delegates_to_body() {
        let template = PromptTemplate {
            name: "component".to_string(),
            path: PathBuf::from("/tmp/component.md"),
            scope: PromptScope::Project,
            description: "Create a component".to_string(),
            argument_hint: Some("<name>".to_string()),
            body: "Create $1 with $2".to_string(),
        };
        assert_eq!(
            template.expand(&args(&["Button", "hooks"])),
            "Create Button with hooks"
        );
    }

    #[test]
    fn expand_invocation_dispatches_on_slash_prefix() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_template(tmp.path(), "greet", "Hello $1");
        let templates = resolver(tmp.path(), vec![(tmp.path(), PromptScope::User)]).catalog();
        // 非 slash 输入：不处理
        assert_eq!(
            expand_invocation(&templates, "just text").expect("ok"),
            None
        );
        // 空格与冒号两种参数形式
        assert_eq!(
            expand_invocation(&templates, "/greet world").expect("ok"),
            Some("Hello world".to_string())
        );
        assert_eq!(
            expand_invocation(&templates, "/greet:world").expect("ok"),
            Some("Hello world".to_string())
        );
        assert_eq!(
            expand_invocation(&templates, "/greet").expect("ok"),
            Some("Hello ".to_string())
        );
        // 未知名称：NotFound 并列出可用模板
        let error = expand_invocation(&templates, "/missing x").expect_err("not found");
        assert!(matches!(error, PromptsError::NotFound { .. }));
        // 引号未闭合：参数错误
        let error = expand_invocation(&templates, "/greet \"x").expect_err("unterminated");
        assert!(matches!(error, PromptsError::UnterminatedQuote { .. }));
    }

    #[test]
    fn resolver_expand_roundtrip() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_template(tmp.path(), "greet", "Hello $1, from ${2:-nomic}");
        let resolver = resolver(tmp.path(), vec![(tmp.path(), PromptScope::User)]);
        assert_eq!(
            resolver.expand("greet", &args(&["world"])).expect("expand"),
            "Hello world, from nomic"
        );
    }
}
