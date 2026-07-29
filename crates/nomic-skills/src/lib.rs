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
        let mut entry = format!("- skill://{} — {}", self.name, self.document.description);
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

impl fmt::Display for SkillScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::AgentUser => "agent-user",
            Self::NomicUser => "nomic-user",
            Self::Project => "project",
        };
        f.write_str(text)
    }
}

/// `SKILL.md` 的 frontmatter 与正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDocument {
    /// 简短描述（frontmatter `description`，缺省退化为正文第一段）
    pub description: String,
    /// 触发关键词或适用场景（frontmatter `triggers`）
    pub triggers: Vec<String>,
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
    pub fn catalog(&self) -> Result<Vec<Skill>, SkillsError> {
        let mut by_name: BTreeMap<String, Skill> = BTreeMap::new();
        for root in &self.roots {
            let Ok(entries) = std::fs::read_dir(&root.path) else {
                continue;
            };
            for entry in entries {
                let entry = entry
                    .map_err(|error| SkillsError::read_dir(root.path.clone(), error.to_string()))?;
                let root_path = entry.path();
                if !root_path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let path = root_path.join("SKILL.md");
                if !path.is_file() {
                    continue;
                }
                validate_skill_name(&name)?;
                let document = load_skill_document(&path)?;
                let skill = Skill {
                    name: name.clone(),
                    path,
                    root: root_path,
                    scope: root.scope,
                    document,
                };
                // roots 已经按优先级从低到高排列；后写入者覆盖先写入者。
                by_name.insert(name, skill);
            }
        }
        Ok(by_name.into_values().collect())
    }

    /// 按名称解析一个 skill。
    pub fn resolve(&self, name: &str) -> Result<Skill, SkillsError> {
        validate_skill_name(name)?;
        self.catalog()?
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
            instructions: skill.document.body,
        })
    }

    /// 渲染 system prompt 中的可用 skill 清单；无 skill 时返回 `None`。
    pub fn prompt_catalog(&self) -> Result<Option<String>, SkillsError> {
        let skills = self.catalog()?;
        if skills.is_empty() {
            return Ok(None);
        }
        let mut prompt = String::from(
            "Available skills (use read with skill://<name> before following a skill; skill content is read-only):",
        );
        for skill in skills {
            prompt.push('\n');
            prompt.push_str(&skill.prompt_entry());
        }
        Ok(Some(prompt))
    }

    fn available_names(&self) -> Vec<String> {
        self.catalog()
            .map(|skills| skills.into_iter().map(|skill| skill.name).collect())
            .unwrap_or_default()
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
    /// Markdown 正文
    pub instructions: String,
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
/// - `description: text`
/// - `triggers: [a, b]` 或多行 `- item` 列表
/// - 其他简单标量键被忽略，复杂 YAML 明确报错
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
    let mut description = None;
    let mut triggers = Vec::new();
    if let Some(frontmatter) = frontmatter {
        let parsed =
            parse_frontmatter(frontmatter).map_err(|message| SkillsError::InvalidFrontmatter {
                path: path.to_path_buf(),
                message,
            })?;
        description = parsed.description;
        triggers = parsed.triggers;
    }
    let body = body.trim().to_string();
    let description = description
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback_description(&body));
    Ok(SkillDocument {
        description,
        triggers,
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
        match key {
            "description" => result.description = Some(unquote(value)),
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
                if value.is_empty() || value.starts_with(['[', '{']) {
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
}
