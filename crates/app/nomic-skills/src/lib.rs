//! nomic-skills：skill 发现、元数据解析与显式激活。
//!
//! skill 是包含 `SKILL.md` 的目录。系统支持三类来源：
//!
//! - 项目级：当前目录向上发现的 `.nomic/skills` 与 `.agents/skills`
//! - nomic 用户级：平台标准配置目录下的 `nomic/skills`（由 `dirs` 解析：
//!   Linux 为 `$XDG_CONFIG_HOME` 或 `~/.config`，macOS 为 `~/Library/Application Support`）
//! - 通用 agent 用户级：`~/.agents/skills`
//!
//! 同名 skill 由更高优先级来源覆盖：`项目级 > nomic 用户级 > 通用 agent 级`；
//! 同级同层中，nomic 专属目录优先于通用 agent 目录。

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use frontmatter::{Frontmatter, fallback_description, parse_frontmatter, split_frontmatter};
use roots::{default_user_roots, discover_project_roots, validate_skill_name};

mod frontmatter;
mod roots;
#[cfg(test)]
mod tests;

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
    /// 用户目录为平台标准配置目录与 `~/.agents`（由 `dirs` 解析）。
    pub fn for_cwd(cwd: &Path) -> Result<Self, SkillsError> {
        tracing::debug!(cwd = %cwd.display(), "skill resolver: initializing for cwd");
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
        tracing::debug!(roots = self.roots.len(), "skill resolver: scanning roots");
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
        tracing::debug!(name = %name, "skill resolver: activating skill");
        let skill = self.resolve(name)?;
        tracing::info!(name = %skill.name, scope = %skill.scope, "skill activated");
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
        tracing::debug!(name = %name, rel = ?rel, "skill resolver: resolving resource");
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
