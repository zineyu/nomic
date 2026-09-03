//! 系统提示词装配（对齐 pi 的结构与措辞）：基础契约 → AGENTS.md（根到叶）→
//! `<available_skills>` → `<internal_uris>` → 激活 skill → `append_system` →
//! 工作目录脚注。`project` 是 session 的操作基准（严格归属）：发现与脚注
//! 都以它为准，而非进程 cwd。

use std::path::Path;

use nomic_skills::{ActivatedSkill, SkillResolver};

use crate::context_files::{ContextFile, discover_agents_files};

/// 系统提示词配方：project 无关的进程级输入（`--append-system` 与
/// `--skill` 激活）。AGENTS.md 按 session 的 project 祖先链发现
///（project 严格归属，与工具基准同口径）；启动（TUI/print）、web 按
/// session project 构建、TUI `/resume` 跨 project 重建，共用同一配方。
#[derive(Debug, Clone, Default)]
pub struct SystemPromptRecipe {
    /// `--append-system` / settings 的追加提示词
    pub append_system: Option<String>,
    /// `--skill` 激活的 skill 列表
    pub active_skills: Vec<ActivatedSkill>,
}

impl SystemPromptRecipe {
    /// 以 `project` 为基准构建完整系统提示词：AGENTS.md 从 project
    /// 沿祖先链发现（根到叶），末尾脚注的工作目录同为 project。
    pub fn build(&self, project: &Path, skill_resolver: &SkillResolver) -> String {
        let context_files = discover_agents_files(project);
        build_system_prompt(
            project,
            self.append_system.as_deref(),
            &context_files,
            skill_resolver,
            &self.active_skills,
        )
    }
}

/// 系统提示词（对齐 pi 的结构与措辞）：基础契约 → AGENTS.md（根到叶）→
/// `append_system` → 当前工作目录脚注。`project` 是 session 的操作基准
///（严格归属）：发现与脚注都以它为准，而非进程 cwd。
fn build_system_prompt(
    project: &Path,
    append: Option<&str>,
    context_files: &[ContextFile],
    skill_resolver: &SkillResolver,
    active_skills: &[ActivatedSkill],
) -> String {
    let mut prompt = "You are an expert coding assistant operating inside nomic, a coding agent harness. \
         You help users by reading files, executing commands, editing code, and writing new files.\n\
         Available tools:\n\
         - read: Read file contents and skill://<name>[/<path>] instructions and resources\n\
         - bash: Execute bash commands\n\
         - grep: Search file contents with a regex (ripgrep-style)\n\
         - find: Find files and directories by glob pattern (fd-style)\n\
         - edit: Make precise file edits with exact text replacement\n\
         - write: Create or overwrite files\n\n\
         Guidelines:\n\
         - Use grep to search file contents and find to locate files\n\
         - Use bash for other shell commands (cargo, git, jj, ls, etc.)\n\
         - Use read to examine files instead of cat or sed\n\
         - Skills are reusable instruction documents; read skill://<name> before following one\n\
         - skill://<name>/<path> reads supporting files (scripts/, references/, etc.) inside the skill directory\n\
         - Do not write or edit skill:// resources; edit their backing files only when the user asks\n\
         - Internal URIs like skill:// accept trailing line selectors, e.g. skill://name/SKILL.md:10-20 or :raw\n\
         - Be concise in your responses\n\
         - Show file paths clearly when working with files"
        .to_string();
    for file in context_files {
        use std::fmt::Write as _;
        let _ = write!(
            prompt,
            "\n\n<project_instructions path=\"{}\">\n{}\n</project_instructions>",
            file.path.display(),
            file.content.trim_end()
        );
    }
    if let Some(catalog) = skill_resolver.prompt_catalog() {
        prompt.push_str("\n\n<available_skills>\n");
        prompt.push_str(&catalog);
        prompt.push_str("\n</available_skills>");
    }
    // 已挂载的内部 URI prefix 目录（ADR-0043）：与工具装配共用
    // fs::session_router，保证提示词与实际挂载集一致
    let vfs_router = nomic_vfs::fs::session_router(
        skill_resolver.clone(),
        nomic_vfs::ProjectRoot::new(Some(project.to_path_buf())),
    );
    if let Some(catalog) = vfs_router.prompt_catalog() {
        prompt.push_str("\n\n<internal_uris>\n");
        prompt.push_str(
            "Internal URI prefixes mounted for this session \
             (usable anywhere a path is accepted; reading a directory lists its entries):\n",
        );
        prompt.push_str(&catalog);
        prompt.push_str("\n</internal_uris>");
    }
    for skill in active_skills {
        prompt.push_str("\n\n");
        prompt.push_str(&skill.prompt_tag());
    }
    if let Some(extra) = append {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
    {
        use std::fmt::Write as _;
        let _ = write!(
            prompt,
            "\n\nCurrent working directory: {}",
            project.display()
        );
    }
    prompt
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn context_file(path: &str, content: &str) -> ContextFile {
        ContextFile {
            path: PathBuf::from(path),
            content: content.to_string(),
        }
    }

    fn empty_skill_resolver() -> SkillResolver {
        SkillResolver::new(
            Path::new("/repo"),
            nomic_skills::ProjectDiscovery::Roots(Vec::new()),
            Vec::new(),
        )
        .expect("empty skill resolver")
    }

    #[test]
    fn prompt_injects_context_files_root_to_leaf() {
        let files = [
            context_file("/repo/AGENTS.md", "root rules"),
            context_file("/repo/sub/AGENTS.md", "sub rules"),
        ];
        let prompt = build_system_prompt(
            Path::new("/repo/sub"),
            None,
            &files,
            &empty_skill_resolver(),
            &[],
        );

        let root_at = prompt.find("root rules").expect("root 内容");
        let sub_at = prompt.find("sub rules").expect("sub 内容");
        assert!(root_at < sub_at, "根到叶顺序");
        // 每份文件都带绝对路径标签
        assert!(prompt.contains("<project_instructions path=\"/repo/AGENTS.md\">"));
        assert!(prompt.contains("<project_instructions path=\"/repo/sub/AGENTS.md\">"));
        assert_eq!(prompt.matches("</project_instructions>").count(), 2);
    }

    #[test]
    fn prompt_keeps_append_and_cwd_without_context_files() {
        let prompt = build_system_prompt(
            Path::new("/repo"),
            Some("额外指令"),
            &[],
            &empty_skill_resolver(),
            &[],
        );
        assert!(!prompt.contains("project_instructions"));
        assert!(prompt.contains("额外指令"));
        assert!(prompt.contains("Current working directory: /repo"));
    }

    #[test]
    fn prompt_orders_base_context_append_cwd() {
        let files = [context_file("/repo/AGENTS.md", "root rules")];
        let prompt = build_system_prompt(
            Path::new("/repo"),
            Some("额外指令"),
            &files,
            &empty_skill_resolver(),
            &[],
        );
        let base_at = prompt.find("Available tools").expect("base");
        let ctx_at = prompt.find("root rules").expect("context");
        let append_at = prompt.find("额外指令").expect("append");
        let cwd_at = prompt.find("Current working directory").expect("cwd");
        assert!(base_at < ctx_at && ctx_at < append_at && append_at < cwd_at);
    }

    /// 配方以 project 为基准：AGENTS.md 从 project 沿祖先链发现
    ///（根到叶），append_system 保留，cwd 脚注同为 project。
    #[test]
    fn recipe_discovers_agents_files_from_project() {
        let root = tempfile::tempdir().expect("tempdir");
        let leaf = root.path().join("sub");
        std::fs::create_dir_all(&leaf).expect("mkdir");
        std::fs::write(root.path().join("AGENTS.md"), "root rules").expect("write root");
        std::fs::write(leaf.join("AGENTS.md"), "leaf rules").expect("write leaf");

        let recipe = SystemPromptRecipe {
            append_system: Some("额外指令".to_string()),
            active_skills: Vec::new(),
        };
        let prompt = recipe.build(&leaf, &empty_skill_resolver());
        let root_at = prompt.find("root rules").expect("root 内容");
        let leaf_at = prompt.find("leaf rules").expect("leaf 内容");
        assert!(root_at < leaf_at, "根到叶顺序");
        assert!(prompt.contains("额外指令"));
        assert!(
            prompt.contains(&format!("Current working directory: {}", leaf.display())),
            "cwd 脚注应跟随 project"
        );
    }

    #[test]
    fn prompt_injects_skill_catalog_and_active_skill() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("skills").join("rust-review");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(
            root.join("SKILL.md"),
            "---\ndescription: Review Rust code\ntriggers: [rust, review]\n---\n# Review\nCheck unsafe code.",
        )
        .expect("write skill");
        let resolver = SkillResolver::new(
            dir.path(),
            nomic_skills::ProjectDiscovery::Roots(Vec::new()),
            vec![nomic_skills::SkillRoot {
                path: dir.path().join("skills"),
                scope: nomic_skills::SkillScope::Project,
            }],
        )
        .expect("resolver");
        let active = resolver.activate("rust-review").expect("activate");

        let prompt = build_system_prompt(
            dir.path(),
            None,
            &[],
            &resolver,
            std::slice::from_ref(&active),
        );
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("skill://rust-review"));
        assert!(prompt.contains("Review Rust code"));
        assert!(prompt.contains("triggers: rust, review"));
        assert!(prompt.contains("<active_skill name=\"rust-review\""));
        assert!(prompt.contains("Check unsafe code."));
        // 注入块带 skill 根目录指引（相对路径解析基准）
        assert!(prompt.contains(&format!("[Skill directory: {}", root.display())));
        // 已挂载的内部 URI prefix 目录（ADR-0043）：prefix + 语义 + 读写性
        assert!(prompt.contains("<internal_uris>"));
        assert!(prompt.contains("- local:// — "), "{prompt}");
        assert!(prompt.contains("(read-write)"), "{prompt}");
        assert!(prompt.contains("- nix:// — "), "{prompt}");
        assert!(prompt.contains("- skill:// — "), "{prompt}");
        assert!(prompt.contains("(read-only)"), "{prompt}");
    }
}
