//! nomic-skills：skill 发现、元数据解析与显式激活。
//!
//! skill 是包含 `SKILL.md` 的目录。系统支持三类来源：
//!
//! - 项目级：当前目录向上发现的 `.nomic/skills` 与 `.agents/skills`
//! - nomic 用户级：`$XDG_CONFIG_HOME/nomic/skills` 与 `~/.config/nomic/skills`
//! - 通用 agent 用户级：`~/.agents/skills`
//!
//! 同名 skill 由更高优先级来源覆盖：`项目级 > nomic 用户级 > 通用 agent 级`；
//! 同级同层中，nomic 专属目录优先于通用 agent 目录。

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// `skill://` URI scheme：`read` 工具与各 prompt 文案共用的唯一定义。
pub const SKILL_SCHEME: &str = "skill://";

/// 已发现的 skill。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// skill 名称（目录名，也是 `skill://name` 中的 name）
    pub name: String,
    /// `SKILL.md` 的真实文件路径
    pub path: PathBuf,
    /// skill 所在目录
    pub root: PathBuf,
    /// skill 来源
    pub scope: SkillScope,
    /// frontmatter / 正文解析结果
    pub document: SkillDocument,
}

impl Skill {
    /// 渲染给 system prompt 的一行清单。
    pub fn prompt_entry(&self) -> String {
        let mut entry = format!(
            "- {SKILL_SCHEME}{} — {}",
            self.name, self.document.description
        );
        if !self.document.triggers.is_empty() {
            let _ = write!(entry, " (triggers: {})", self.document.triggers.join(", "));
        }
        entry
    }
}

/// skill 来源层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillScope {
    /// 通用 agent 用户目录
    AgentUser,
    /// nomic 用户配置目录
    NomicUser,
    /// 项目目录（含向上继承的项目目录）
    Project,
}

impl SkillScope {
    /// 序列化到 prompt / 标签的稳定文本形式。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentUser => "agent-user",
            Self::NomicUser => "nomic-user",
            Self::Project => "project",
        }
    }
}

impl fmt::Display for SkillScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SkillScope {
    type Err = SkillsError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "agent-user" => Ok(Self::AgentUser),
            "nomic-user" => Ok(Self::NomicUser),
            "project" => Ok(Self::Project),
            _ => Err(SkillsError::InvalidScope {
                scope: text.to_string(),
            }),
        }
    }
}

/// `SKILL.md` 的 frontmatter 与正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDocument {
    /// 简短描述（frontmatter `description`，缺省退化为正文第一段）
    pub description: String,
    /// 触发关键词或适用场景（frontmatter `triggers`）
    pub triggers: Vec<String>,
    /// frontmatter `enabled: false` 时整个 skill 不可用（catalog 与 resolve 均跳过）
    pub enabled: bool,
    /// frontmatter `hide: true` 时可 resolve / 激活，但不出现在 prompt 清单
    /// （用于只供显式调用的 skill，对齐 omp 的 hide / disable-model-invocation）
    pub hide: bool,
    /// 去掉 frontmatter 后的 Markdown 正文
    pub body: String,
}

/// 项目级目录发现规则（预留显式 roots 模式，便于测试和自定义集成）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectDiscovery {
    /// 从当前 cwd 向上查找到文件系统根
    Ancestors,
    /// 只使用显式提供的一组项目根
    Roots(Vec<PathBuf>),
}

/// skill 解析与激活入口。
#[derive(Debug, Clone)]
pub struct SkillResolver {
    roots: Vec<SkillRoot>,
}

impl SkillResolver {
    /// 按当前 cwd 构造标准 resolver。
    ///
    /// 项目目录从 `cwd` 向上查找 `.nomic/skills` 与 `.agents/skills`；
    /// 用户目录按 XDG / HOME 解析。`HOME` 缺失时仍允许只使用 XDG 用户目录。
    pub fn for_cwd(cwd: &Path) -> Result<Self, SkillsError> {
        Self::new(cwd, ProjectDiscovery::Ancestors, default_user_roots())
    }

    /// 显式构造 resolver（测试与自定义目录）。
    ///
    /// `ProjectDiscovery::Roots` 中的根应按低优先级到高优先级传入；
    /// `ProjectDiscovery::Ancestors` 自动保证越靠近 cwd 优先级越高。
    pub fn new(
        cwd: &Path,
        project_discovery: ProjectDiscovery,
        user_roots: Vec<SkillRoot>,
    ) -> Result<Self, SkillsError> {
        if !cwd.is_absolute() {
            return Err(SkillsError::RelativeCwd(cwd.to_path_buf()));
        }
        let project_roots = discover_project_roots(cwd, project_discovery);
        // roots 按低优先级到高优先级排列（catalog 中后写入者覆盖先写入者）：
        // 用户级在前，项目级在后，保证项目级 skill 覆盖同名用户级 skill。
        let mut roots = user_roots;
        roots.extend(project_roots);
        Ok(Self { roots })
    }

    /// 发现并按覆盖规则返回全部可用 skill。
    ///
    /// 加载失败的单个 skill（名称非法、文件不可读、frontmatter 非法）会被跳过，
    /// 不影响其他 skill；需要诊断信息时使用 [`Self::catalog_with_diagnostics`]。
    pub fn catalog(&self) -> Vec<Skill> {
        self.catalog_with_diagnostics().skills
    }

    /// 同 [`Self::catalog`]，同时返回被跳过 skill 的诊断信息。
    pub fn catalog_with_diagnostics(&self) -> SkillCatalog {
        let mut by_name: BTreeMap<String, Skill> = BTreeMap::new();
        let mut errors = Vec::new();
        for root in &self.roots {
            let Ok(entries) = std::fs::read_dir(&root.path) else {
                continue;
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        errors.push(SkillsError::read_dir(root.path.clone(), error.to_string()));
                        continue;
                    }
                };
                let root_path = entry.path();
                if !root_path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = root_path.join("SKILL.md");
                if !path.is_file() {
                    continue;
                }
                let skill =
                    match validate_skill_name(&name).and_then(|()| load_skill_document(&path)) {
                        Ok(document) => {
                            if !document.enabled {
                                // enabled: false —— 静默跳过（用户显式关闭，非加载错误）
                                continue;
                            }
                            Skill {
                                name: name.clone(),
                                path,
                                root: root_path,
                                scope: root.scope,
                                document,
                            }
                        }
                        Err(error) => {
                            errors.push(error);
                            continue;
                        }
                    };
                // roots 已经按优先级从低到高排列；后写入者覆盖先写入者。
                by_name.insert(name, skill);
            }
        }
        SkillCatalog {
            skills: by_name.into_values().collect(),
            errors,
        }
    }

    /// 按名称解析一个 skill。
    pub fn resolve(&self, name: &str) -> Result<Skill, SkillsError> {
        validate_skill_name(name)?;
        self.catalog()
            .into_iter()
            .find(|skill| skill.name == name)
            .ok_or_else(|| SkillsError::not_found(name, self.available_names()))
    }

    /// 显式激活一个 skill，返回正文（不含 frontmatter）。
    pub fn activate(&self, name: &str) -> Result<ActivatedSkill, SkillsError> {
        let skill = self.resolve(name)?;
        Ok(ActivatedSkill {
            name: skill.name,
            scope: skill.scope,
            path: skill.path,
            root: skill.root,
            instructions: skill.document.body,
        })
    }

    /// 解析 `skill://<name>[/<path>]` 资源。
    ///
    /// `rel` 为空（或只有 `.`）时返回 [`SkillResource::Instructions`]（SKILL.md 正文）；
    /// 否则按词法规范化相对路径并要求落在 skill 根目录内，存在时按实际类型返回
    /// 文件或目录资源。不做符号链接解析：穿越防护只保证词法前缀，skill 目录内
    /// 指向外部的符号链接不在此拦截（skill 对用户是可信资产）。
    pub fn resolve_resource(
        &self,
        name: &str,
        rel: Option<&str>,
    ) -> Result<SkillResource, SkillsError> {
        let skill = self.resolve(name)?;
        let Some(rel) = rel else {
            return Ok(SkillResource::Instructions(skill));
        };
        let Some(path) = resolve_resource_path(&skill.root, rel).map_err(|()| {
            SkillsError::InvalidResourcePath {
                name: name.to_string(),
                path: rel.to_string(),
            }
        })?
        else {
            return Ok(SkillResource::Instructions(skill));
        };
        if path.is_file() {
            Ok(SkillResource::File { skill, path })
        } else if path.is_dir() {
            Ok(SkillResource::Directory { skill, path })
        } else {
            Err(SkillsError::ResourceNotFound {
                name: name.to_string(),
                path: rel.to_string(),
            })
        }
    }

    /// 渲染 system prompt 中的可用 skill 清单；无可见 skill 时返回 `None`。
    ///
    /// `hide: true` 的 skill 不在清单出现（仍可显式 resolve / 激活）。
    pub fn prompt_catalog(&self) -> Option<String> {
        let skills: Vec<Skill> = self
            .catalog()
            .into_iter()
            .filter(|skill| !skill.document.hide)
            .collect();
        if skills.is_empty() {
            return None;
        }
        let mut prompt = format!(
            "Available skills (use read with {SKILL_SCHEME}<name> before following a skill; \
             skill content is read-only; {SKILL_SCHEME}<name>/<path> reads files inside the \
             skill directory, such as scripts/ or references/):"
        );
        for skill in skills {
            prompt.push('\n');
            prompt.push_str(&skill.prompt_entry());
        }
        Some(prompt)
    }

    fn available_names(&self) -> Vec<String> {
        self.catalog().into_iter().map(|skill| skill.name).collect()
    }
}

/// 显式激活后的 skill 指令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivatedSkill {
    /// skill 名称
    pub name: String,
    /// 来源层级
    pub scope: SkillScope,
    /// `SKILL.md` 路径
    pub path: PathBuf,
    /// skill 根目录（附属资源的相对路径解析基准）
    pub root: PathBuf,
    /// Markdown 正文
    pub instructions: String,
}

impl ActivatedSkill {
    /// 渲染 `<active_skill>` 注入标签。
    ///
    /// system prompt（`--skill`）与会话内注入（`/skill:<name>`）共用同一格式，
    /// 解析侧见 [`parse_active_skill_tag`]。块尾附加 skill 根目录指引，让模型
    /// 能解析正文中引用的相对路径并按需读取附属资源（对齐 omp 的 baseDir 注入）。
    pub fn prompt_tag(&self) -> String {
        format!(
            "<active_skill name=\"{}\" scope=\"{}\" path=\"{}\">\n{}\n\n\
             [Skill directory: {} — relative paths referenced by this skill resolve against \
             this directory; read its files via {SKILL_SCHEME}{}/<path> or the filesystem, \
             and run its scripts with bash, as needed.]\n</active_skill>",
            self.name,
            self.scope,
            self.path.display(),
            self.instructions,
            self.root.display(),
            self.name,
        )
    }
}

/// 从 `<active_skill ...>` 标签头解析出的属性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSkillTag {
    /// skill 名称（缺失视为非 skill 注入文本）
    pub name: String,
    /// 来源层级（旧格式可能缺失）
    pub scope: Option<SkillScope>,
    /// `SKILL.md` 路径（旧格式可能缺失）
    pub path: Option<PathBuf>,
}

/// 解析 [`ActivatedSkill::prompt_tag`] 生成的注入文本。
///
/// 仅识别以 `<active_skill ` 开头的文本；属性解析自开头标签头（首个 `>` 之前）。
/// 解析失败返回 `None`，调用方应回退为普通文本处理。
pub fn parse_active_skill_tag(text: &str) -> Option<ActiveSkillTag> {
    let header = text.strip_prefix("<active_skill ")?;
    let header = &header[..header.find('>')?];
    let name = tag_attr(header, "name")?;
    let scope = tag_attr(header, "scope").and_then(|value| value.parse().ok());
    let path = tag_attr(header, "path").map(PathBuf::from);
    Some(ActiveSkillTag { name, scope, path })
}

/// 从标签头中提取 `key="value"` 属性值。
fn tag_attr(header: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = header.find(&needle)? + needle.len();
    let end = header[start..].find('"')? + start;
    Some(header[start..end].to_string())
}

/// `skill://<name>[/<path>]` 解析出的资源。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillResource {
    /// 无子路径：`SKILL.md` 正文（不含 frontmatter）
    Instructions(Skill),
    /// skill 根目录内的文件（词法规范化后的绝对路径）
    File {
        /// 所属 skill
        skill: Skill,
        /// 资源绝对路径
        path: PathBuf,
    },
    /// skill 根目录内的目录
    Directory {
        /// 所属 skill
        skill: Skill,
        /// 资源绝对路径
        path: PathBuf,
    },
}

/// 词法规范化 `root.join(rel)`，越出 `root` 返回 `Err(())`，
/// 空路径（或只有 `.`）返回 `Ok(None)`。
///
/// 只处理 `Normal` / `CurDir` / `ParentDir` 组件：拒绝绝对路径与 Windows
/// 前缀；`..` 弹出深度，弹到 0 以下即越界。不触碰文件系统，因此结果稳定、
/// 与路径是否存在无关。
fn resolve_resource_path(root: &Path, rel: &str) -> Result<Option<PathBuf>, ()> {
    let mut path = root.to_path_buf();
    let mut depth = 0usize;
    for component in Path::new(rel).components() {
        match component {
            std::path::Component::Normal(part) => {
                path.push(part);
                depth += 1;
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if depth == 0 {
                    return Err(());
                }
                path.pop();
                depth -= 1;
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return Err(()),
        }
    }
    Ok((depth > 0).then_some(path))
}

/// [`SkillResolver::catalog_with_diagnostics`] 的结果。
#[derive(Debug)]
pub struct SkillCatalog {
    /// 成功加载的 skill（已按覆盖规则去重）
    pub skills: Vec<Skill>,
    /// 加载失败被跳过的 skill 及原因
    pub errors: Vec<SkillsError>,
}

/// 一个 skill 根目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRoot {
    /// 根目录路径（其中每个子目录是一个 skill）
    pub path: PathBuf,
    /// 来源层级
    pub scope: SkillScope,
}

/// skills 系统错误。
#[derive(Debug, thiserror::Error)]
pub enum SkillsError {
    /// cwd 必须是绝对路径
    #[error("skill resolver requires an absolute current directory, got {}", .0.display())]
    RelativeCwd(PathBuf),
    /// skill 名称非法
    #[error(
        "invalid skill name {name:?}; use 1-64 chars of lowercase ASCII letters, digits, '-' or '_' (cannot start/end with '-' or '_')"
    )]
    InvalidName {
        /// 非法名称
        name: String,
    },
    /// 找不到指定 skill
    #[error("skill {name:?} not found{}", if available.is_empty() { String::new() } else { format!("; available: {}", available.join(", ")) })]
    NotFound {
        /// 请求的名称
        name: String,
        /// 当前可用名称
        available: Vec<String>,
    },
    /// skill 来源层级文本非法
    #[error("invalid skill scope {scope:?}; expected one of: agent-user, nomic-user, project")]
    InvalidScope {
        /// 非法文本
        scope: String,
    },
    /// 读取 `SKILL.md` 失败
    #[error("failed to read skill file {}: {message}", .path.display())]
    ReadSkillFile {
        /// 文件路径
        path: PathBuf,
        /// 底层错误
        message: String,
    },
    /// frontmatter 非法
    #[error("invalid skill frontmatter in {}: {message}", .path.display())]
    InvalidFrontmatter {
        /// 文件路径
        path: PathBuf,
        /// 错误说明
        message: String,
    },
    /// skill 子资源路径非法（绝对路径或穿越出 skill 根目录）
    #[error(
        "invalid resource path {path:?} for skill {name:?}; path must stay inside the skill directory"
    )]
    InvalidResourcePath {
        /// skill 名称
        name: String,
        /// 非法的相对路径
        path: String,
    },
    /// skill 子资源不存在
    #[error("resource {path:?} not found in skill {name:?}")]
    ResourceNotFound {
        /// skill 名称
        name: String,
        /// 相对路径
        path: String,
    },
    /// 目录扫描失败
    #[error("failed to scan skills directory {}: {message}", .path.display())]
    ReadDir {
        /// 目录路径
        path: PathBuf,
        /// 底层错误
        message: String,
    },
}

impl SkillsError {
    fn not_found(name: &str, available: Vec<String>) -> Self {
        Self::NotFound {
            name: name.to_string(),
            available,
        }
    }

    const fn read_dir(path: PathBuf, message: String) -> Self {
        Self::ReadDir { path, message }
    }
}

/// 默认用户级 skill 根（低优先级在前，高优先级在后）。
fn default_user_roots() -> Vec<SkillRoot> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        roots.push(SkillRoot {
            path: PathBuf::from(&home).join(".agents").join("skills"),
            scope: SkillScope::AgentUser,
        });
        roots.push(SkillRoot {
            path: PathBuf::from(&home)
                .join(".config")
                .join("nomic")
                .join("skills"),
            scope: SkillScope::NomicUser,
        });
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        roots.push(SkillRoot {
            path: PathBuf::from(xdg).join("nomic").join("skills"),
            scope: SkillScope::NomicUser,
        });
    }
    roots
}

/// 发现项目级 skill 根。返回顺序从低优先级到高优先级。
fn discover_project_roots(cwd: &Path, discovery: ProjectDiscovery) -> Vec<SkillRoot> {
    // 统一按低优先级到高优先级遍历项目根：Ancestors 模式从文件系统根到 cwd
    //（越靠近 cwd 越优先），Roots 模式由调用方按低优先级到高优先级传入。
    let roots = match discovery {
        ProjectDiscovery::Ancestors => {
            let mut ancestors = cwd.ancestors().map(Path::to_path_buf).collect::<Vec<_>>();
            ancestors.reverse();
            ancestors
        }
        ProjectDiscovery::Roots(roots) => roots,
    };
    let mut discovered = Vec::new();
    for root in roots {
        // 同级同层中 .nomic/skills 优先于 .agents/skills（后写入者覆盖先写入者）。
        discovered.push(SkillRoot {
            path: root.join(".agents").join("skills"),
            scope: SkillScope::Project,
        });
        discovered.push(SkillRoot {
            path: root.join(".nomic").join("skills"),
            scope: SkillScope::Project,
        });
    }
    discovered
}

/// 校验 skill 名称，避免路径穿越与 URI 歧义。
fn validate_skill_name(name: &str) -> Result<(), SkillsError> {
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
        Err(SkillsError::InvalidName {
            name: name.to_string(),
        })
    }
}

/// 加载 `SKILL.md` 并解析可选 YAML frontmatter 的最小兼容子集。
///
/// 为避免为 skill 文档引入完整 YAML 依赖，当前支持：
/// - `description: text`，或块标量形式 `description: >-` / `|` 加缩进续行
/// - `triggers: [a, b]` 或多行 `- item` 列表
/// - `enabled: true/false`（false 时整个 skill 不可用）与 `hide: true/false`
///   （不出现在 prompt 清单，仍可显式调用）
/// - 其他简单标量键被忽略；未知键的嵌套块（如 `metadata:` 下的 map）被跳过；
///   其余复杂 YAML 明确报错
fn load_skill_document(path: &Path) -> Result<SkillDocument, SkillsError> {
    let text = std::fs::read_to_string(path).map_err(|error| SkillsError::ReadSkillFile {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let (frontmatter, body) =
        split_frontmatter(&text).map_err(|message| SkillsError::InvalidFrontmatter {
            path: path.to_path_buf(),
            message,
        })?;
    let parsed = if let Some(frontmatter) = frontmatter {
        parse_frontmatter(frontmatter).map_err(|message| SkillsError::InvalidFrontmatter {
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
    Ok(SkillDocument {
        description,
        triggers: parsed.triggers,
        enabled: parsed.enabled.unwrap_or(true),
        hide: parsed.hide.unwrap_or(false),
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
    triggers: Vec<String>,
    enabled: Option<bool>,
    hide: Option<bool>,
}

/// 解析布尔标量（`true` / `false`，可带引号）。
fn parse_bool(key: &str, value: &str) -> Result<bool, String> {
    match unquote(value).as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!(
            "frontmatter field {key:?} expects true/false, got {other:?}"
        )),
    }
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
            "enabled" => result.enabled = Some(parse_bool(key, value)?),
            "hide" => result.hide = Some(parse_bool(key, value)?),
            "triggers" => {
                if value.is_empty() {
                    while let Some(next) = lines.peek() {
                        let item = next.trim();
                        if item.is_empty() {
                            lines.next();
                            continue;
                        }
                        let Some(item) = item.strip_prefix("- ") else {
                            break;
                        };
                        result.triggers.push(unquote(item.trim()));
                        lines.next();
                    }
                } else if value.starts_with('[') && value.ends_with(']') {
                    let inner = &value[1..value.len() - 1];
                    if !inner.trim().is_empty() {
                        result
                            .triggers
                            .extend(inner.split(',').map(|item| unquote(item.trim())));
                    }
                } else {
                    result.triggers.push(unquote(value));
                }
            }
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
    result.triggers.retain(|trigger| !trigger.is_empty());
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
mod tests {
    use super::*;

    fn temp_skill(root: &Path, name: &str, text: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("SKILL.md"), text).expect("write");
    }

    fn resolver(cwd: &Path, roots: Vec<(&Path, SkillScope)>) -> SkillResolver {
        SkillResolver::new(
            cwd,
            ProjectDiscovery::Roots(Vec::new()),
            roots
                .into_iter()
                .map(|(path, scope)| SkillRoot {
                    path: path.to_path_buf(),
                    scope,
                })
                .collect(),
        )
        .expect("resolver")
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_skill(
            tmp.path(),
            "rust-review",
            "---\ndescription: Review Rust changes\ntriggers: [rust, review]\n---\n# Steps\nCheck it.\n",
        );
        let skill = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)])
            .resolve("rust-review")
            .expect("resolve");
        assert_eq!(skill.document.description, "Review Rust changes");
        assert_eq!(skill.document.triggers, vec!["rust", "review"]);
        assert_eq!(skill.document.body, "# Steps\nCheck it.");
    }

    #[test]
    fn higher_priority_root_overrides_same_name() {
        let tmp = tempfile::tempdir().expect("tmp");
        let low = tmp.path().join("low");
        let high = tmp.path().join("high");
        temp_skill(&low, "shared", "low body");
        temp_skill(&high, "shared", "high body");
        let skill = resolver(
            tmp.path(),
            vec![
                (low.as_path(), SkillScope::AgentUser),
                (high.as_path(), SkillScope::Project),
            ],
        )
        .resolve("shared")
        .expect("resolve");
        assert_eq!(skill.document.body, "high body");
        assert_eq!(skill.scope, SkillScope::Project);
    }

    #[test]
    fn rejects_path_traversal_name() {
        let tmp = tempfile::tempdir().expect("tmp");
        let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)]);
        let error = resolver.resolve("../secret").expect_err("invalid");
        assert!(matches!(error, SkillsError::InvalidName { .. }));
    }

    #[test]
    fn resolves_skill_sub_resources_with_traversal_guard() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path().join("demo");
        std::fs::create_dir_all(root.join("scripts")).expect("mkdir");
        std::fs::write(root.join("SKILL.md"), "demo body").expect("write");
        std::fs::write(root.join("scripts/run.sh"), "#!/bin/sh\n").expect("write");
        std::fs::write(tmp.path().join("secret.txt"), "secret").expect("write");
        let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)]);

        // 无子路径 / 空子路径：返回 skill 正文指令
        for rel in [None, Some(""), Some(".")] {
            let resource = resolver
                .resolve_resource("demo", rel)
                .expect("instructions");
            assert!(matches!(resource, SkillResource::Instructions(_)));
        }

        // 文件子资源：返回规范化后的绝对路径
        let resource = resolver
            .resolve_resource("demo", Some("scripts//run.sh"))
            .expect("file");
        let SkillResource::File { path, .. } = resource else {
            panic!("expected file resource");
        };
        assert_eq!(path, root.join("scripts/run.sh"));

        // 目录子资源
        let resource = resolver
            .resolve_resource("demo", Some("scripts"))
            .expect("dir");
        assert!(matches!(resource, SkillResource::Directory { .. }));

        // 穿越到 skill 根之外：拒绝（含经中间目录折返的情形）
        for rel in ["../secret.txt", "scripts/../../secret.txt", "/etc/passwd"] {
            let error = resolver
                .resolve_resource("demo", Some(rel))
                .expect_err("traversal");
            assert!(matches!(error, SkillsError::InvalidResourcePath { .. }));
        }

        // 根内不存在的路径
        let error = resolver
            .resolve_resource("demo", Some("scripts/missing.sh"))
            .expect_err("missing");
        assert!(matches!(error, SkillsError::ResourceNotFound { .. }));
    }

    #[test]
    fn fallback_description_uses_first_heading() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_skill(tmp.path(), "plain", "\n# Do useful work\nBody\n");
        let skill = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)])
            .resolve("plain")
            .expect("resolve");
        assert_eq!(skill.document.description, "Do useful work");
    }

    #[test]
    fn project_skill_overrides_user_skill() {
        let tmp = tempfile::tempdir().expect("tmp");
        let project = tmp.path().join("project");
        let user = tmp.path().join("user");
        temp_skill(&project.join(".nomic/skills"), "shared", "project body");
        temp_skill(&user, "shared", "user body");
        let resolver = SkillResolver::new(
            &project,
            ProjectDiscovery::Roots(vec![project.clone()]),
            vec![SkillRoot {
                path: user,
                scope: SkillScope::NomicUser,
            }],
        )
        .expect("resolver");
        let skill = resolver.resolve("shared").expect("resolve");
        assert_eq!(skill.document.body, "project body");
        assert_eq!(skill.scope, SkillScope::Project);
    }

    #[test]
    fn ancestors_mode_prefers_nearer_dir_and_nomic_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        temp_skill(&outer.join(".agents/skills"), "shared", "outer agents");
        temp_skill(&outer.join(".nomic/skills"), "shared", "outer nomic");
        temp_skill(&inner.join(".agents/skills"), "shared", "inner agents");
        temp_skill(&inner.join(".nomic/skills"), "shared", "inner nomic");
        // 只在父级 .agents 与 .nomic 中同时存在：同层 .nomic 应优先。
        temp_skill(&outer.join(".agents/skills"), "outer-only", "outer agents");
        temp_skill(&outer.join(".nomic/skills"), "outer-only", "outer nomic");
        let resolver =
            SkillResolver::new(&inner, ProjectDiscovery::Ancestors, Vec::new()).expect("resolver");
        assert_eq!(
            resolver.resolve("shared").expect("resolve").document.body,
            "inner nomic"
        );
        assert_eq!(
            resolver
                .resolve("outer-only")
                .expect("resolve")
                .document
                .body,
            "outer nomic"
        );
    }

    #[test]
    fn roots_mode_prefers_later_root_and_nomic_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        let low = tmp.path().join("low");
        let high = tmp.path().join("high");
        temp_skill(&low.join(".nomic/skills"), "shared", "low nomic");
        temp_skill(&high.join(".agents/skills"), "shared", "high agents");
        temp_skill(&high.join(".nomic/skills"), "shared", "high nomic");
        let resolver = SkillResolver::new(
            tmp.path(),
            ProjectDiscovery::Roots(vec![low, high]),
            Vec::new(),
        )
        .expect("resolver");
        assert_eq!(
            resolver.resolve("shared").expect("resolve").document.body,
            "high nomic"
        );
    }

    #[test]
    fn parses_block_scalar_description_and_nested_unknown_fields() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_skill(
            tmp.path(),
            "folded",
            "---\nname: folded\ndescription: >-\n  First line\n  second line\nlicense: MIT\nmetadata:\n  category: test\n  version: \"1.0\"\n---\nBody\n",
        );
        temp_skill(
            tmp.path(),
            "literal",
            "---\ndescription: |\n  line one\n  line two\n---\nBody\n",
        );
        let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)]);
        let folded = resolver.resolve("folded").expect("resolve folded");
        assert_eq!(folded.document.description, "First line second line");
        assert_eq!(folded.document.body, "Body");
        let literal = resolver.resolve("literal").expect("resolve literal");
        assert_eq!(literal.document.description, "line one\nline two");
    }

    #[test]
    fn active_skill_tag_roundtrips_and_rejects_plain_text() {
        let skill = ActivatedSkill {
            name: "rust-review".to_string(),
            scope: SkillScope::Project,
            path: PathBuf::from("/repo/.nomic/skills/rust-review/SKILL.md"),
            root: PathBuf::from("/repo/.nomic/skills/rust-review"),
            instructions: "# Review\nCheck unsafe code.".to_string(),
        };
        let tag = skill.prompt_tag();
        assert!(tag.starts_with(
            "<active_skill name=\"rust-review\" scope=\"project\" \
             path=\"/repo/.nomic/skills/rust-review/SKILL.md\">"
        ));
        // 注入块尾部带 skill 根目录指引：相对路径的解析基准 + 子资源读取方式
        assert!(tag.ends_with(
            "# Review\nCheck unsafe code.\n\n\
             [Skill directory: /repo/.nomic/skills/rust-review — relative paths referenced \
             by this skill resolve against this directory; read its files via \
             skill://rust-review/<path> or the filesystem, and run its scripts with bash, \
             as needed.]\n</active_skill>"
        ));

        // 标签后允许拼接其他文本（如会话内注入的说明）。
        let parsed = parse_active_skill_tag(&format!("{tag}\n\nmanual note")).expect("parse");
        assert_eq!(parsed.name, "rust-review");
        assert_eq!(parsed.scope, Some(SkillScope::Project));
        assert_eq!(
            parsed.path,
            Some(PathBuf::from("/repo/.nomic/skills/rust-review/SKILL.md"))
        );

        // 旧格式缺 scope / path 时仍可解析出 name。
        let legacy =
            parse_active_skill_tag("<active_skill name=\"legacy\">\nbody").expect("legacy");
        assert_eq!(legacy.name, "legacy");
        assert_eq!(legacy.scope, None);
        assert_eq!(legacy.path, None);

        assert!(parse_active_skill_tag("plain text").is_none());
        assert!(parse_active_skill_tag("<active_skill scope=\"project\">").is_none());
        assert!("garbage".parse::<SkillScope>().is_err());
    }

    #[test]
    fn frontmatter_enabled_and_hide_control_visibility() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_skill(tmp.path(), "normal", "normal body");
        temp_skill(tmp.path(), "off", "---\nenabled: false\n---\noff body");
        temp_skill(tmp.path(), "hidden", "---\nhide: true\n---\nhidden body");
        let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)]);

        // enabled: false —— 彻底跳过，resolve 也找不到
        let error = resolver.resolve("off").expect_err("disabled");
        assert!(matches!(error, SkillsError::NotFound { .. }));

        // hide: true —— 可 resolve / 激活，但不出现在 prompt 清单
        assert_eq!(
            resolver
                .resolve("hidden")
                .expect("resolve hidden")
                .document
                .body,
            "hidden body"
        );
        let prompt = resolver.prompt_catalog().expect("non-empty");
        assert!(prompt.contains("skill://normal"));
        assert!(!prompt.contains("hidden"));
        assert!(!prompt.contains("skill://off"));

        // 非布尔值：frontmatter 非法，skill 被跳过并记录诊断
        temp_skill(tmp.path(), "bad-bool", "---\nenabled: maybe\n---\nbody");
        let catalog = resolver.catalog_with_diagnostics();
        assert!(catalog.skills.iter().all(|skill| skill.name != "bad-bool"));
        assert!(
            catalog
                .errors
                .iter()
                .any(|error| matches!(error, SkillsError::InvalidFrontmatter { .. }))
        );
    }

    #[test]
    fn broken_skill_is_skipped_without_breaking_catalog() {
        let tmp = tempfile::tempdir().expect("tmp");
        temp_skill(tmp.path(), "good", "good body");
        temp_skill(
            tmp.path(),
            "broken",
            "---\nmetadata: {nested: flow}\n---\nbody\n",
        );
        // 非法名称的目录同样只被跳过。
        temp_skill(tmp.path(), "BadName", "bad name body");
        let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::AgentUser)]);

        let catalog = resolver.catalog_with_diagnostics();
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "good");
        assert_eq!(catalog.errors.len(), 2);
        assert!(
            catalog
                .errors
                .iter()
                .any(|error| matches!(error, SkillsError::InvalidFrontmatter { .. }))
        );
        assert!(
            catalog
                .errors
                .iter()
                .any(|error| matches!(error, SkillsError::InvalidName { .. }))
        );

        // resolve / prompt_catalog 均不受坏 skill 影响。
        assert_eq!(
            resolver.resolve("good").expect("resolve").document.body,
            "good body"
        );
        let prompt = resolver.prompt_catalog().expect("non-empty");
        assert!(prompt.contains("skill://good"));
        assert!(!prompt.contains("broken"));
        // 清单头部说明子资源读取方式
        assert!(prompt.contains("skill://<name>/<path>"));
    }
}
