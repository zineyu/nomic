//! nomic-prompts：prompt template 发现、元数据解析与参数展开。
//!
//! prompt template 是一个 `.md` 文件：文件名（去掉 `.md`）即命令名，正文是模板。
//! 在编辑器中输入 `/name 参数...`，模板展开为完整 prompt 后提交。
//!
//! 支持三类来源（低优先级 → 高优先级，同名覆盖）：
//!
//! - 用户级：平台标准配置目录下的 `nomic/prompts/*.md`（由 `dirs` 解析：
//!   Linux 为 `$XDG_CONFIG_HOME` 或 `~/.config`，macOS 为 `~/Library/Application Support`）
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

mod expand;
mod frontmatter;

pub use expand::{expand_invocation, expand_template, split_arguments};
use frontmatter::{Frontmatter, fallback_description, parse_frontmatter, split_frontmatter};

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
    /// 项目目录从 `cwd` 向上查找 `.nomic/prompts`；用户目录为平台标准配置
    /// 目录（由 `dirs` 解析），无法解析时仅使用项目级目录。
    pub fn for_cwd(cwd: &Path) -> Result<Self, PromptsError> {
        tracing::debug!(cwd = %cwd.display(), "prompt resolver: initializing for cwd");
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
        tracing::debug!(
            roots = self.roots.len(),
            explicit = self.explicit.len(),
            "prompt resolver: scanning"
        );
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

/// 默认用户级模板根：平台标准配置目录下的 `nomic/prompts`（由 `dirs` 解析：
/// Linux 为 `$XDG_CONFIG_HOME` 或 `~/.config`，macOS 为 `~/Library/Application Support`）。
fn default_user_roots() -> Vec<PromptRoot> {
    dirs::config_dir()
        .map(|dir| PromptRoot {
            path: dir.join("nomic").join("prompts"),
            scope: PromptScope::User,
        })
        .into_iter()
        .collect()
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

#[cfg(test)]
mod tests;
