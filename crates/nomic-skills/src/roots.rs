use std::path::{Path, PathBuf};

use super::{ProjectDiscovery, SkillRoot, SkillScope, SkillsError};

/// 默认用户级 skill 根（低优先级在前，高优先级在后）。
pub fn default_user_roots() -> Vec<SkillRoot> {
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
pub fn discover_project_roots(cwd: &Path, discovery: ProjectDiscovery) -> Vec<SkillRoot> {
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
pub fn validate_skill_name(name: &str) -> Result<(), SkillsError> {
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
