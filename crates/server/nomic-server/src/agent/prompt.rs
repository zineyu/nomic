//! 系统提示组装。
//!
//! 组装是工作目录的确定性函数：内置默认提示 + 存在即注入的
//! AGENTS.md / CLAUDE.md。原始执行与恢复重放对同一工作目录
//! 产出同一提示，LLM 请求历史因此可精确回放。

use std::fs;
use std::io;
use std::path::Path;

/// 按序注入系统提示的上下文文件名。
const CONTEXT_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// 内置默认系统提示：角色、协作方式与边界约束。
///
/// 提示文本本体在同名 markdown 文件中维护（内容演进与代码解耦），
/// 此处经 `include_str!` 引入。
pub const DEFAULT_SYSTEM_PROMPT: &str = include_str!("default_system_prompt.md");

/// 组装完整系统提示文本：内置默认 + 存在的上下文文件节。
///
/// 形态：`DEFAULT_SYSTEM_PROMPT`（拼接边界做空白规整）+ 每个存在且
/// 非空的上下文文件以固定节注入（按 `CONTEXT_FILES` 声明顺序）。
/// 文件不存在不是错误（未注入项目即无该约定）；文件存在但为空
/// 同样不注入。读取失败（权限等部署故障）以 [`io::Error`] 传播，
/// 不静默降级。
///
/// # Errors
///
/// 工作目录不可读（权限等部署故障）时返回底层 [`io::Error`]。
pub fn assemble_system_prompt(working_dir: &Path) -> io::Result<String> {
    let mut prompt = String::from(DEFAULT_SYSTEM_PROMPT.trim_end());
    for name in CONTEXT_FILES {
        let content = match fs::read_to_string(working_dir.join(name)) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if content.trim().is_empty() {
            continue;
        }
        prompt.push_str("\n\n## ");
        prompt.push_str(name);
        prompt.push_str("\n\n");
        prompt.push_str(content.trim());
    }
    Ok(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 无上下文文件_提示等于内置默认() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            assemble_system_prompt(dir.path()).unwrap(),
            DEFAULT_SYSTEM_PROMPT.trim_end()
        );
    }

    #[test]
    fn 默认提示拼接边界做空白规整() {
        // 文件尾换行不进入组装结果，与上下文内容的 trim 行为一致
        assert!(
            !assemble_system_prompt(Path::new("/nonexistent"))
                .unwrap()
                .ends_with(char::is_whitespace)
        );
    }

    #[test]
    fn 上下文文件按固定节注入且保持顺序() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "项目规约\n").unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "回答使用中文").unwrap();
        let prompt = assemble_system_prompt(dir.path()).unwrap();
        assert!(prompt.starts_with(DEFAULT_SYSTEM_PROMPT));
        assert!(prompt.contains("## AGENTS.md\n\n项目规约"));
        assert!(prompt.contains("## CLAUDE.md\n\n回答使用中文"));
        assert!(
            prompt.find("## AGENTS.md").unwrap() < prompt.find("## CLAUDE.md").unwrap(),
            "AGENTS.md 节必须先于 CLAUDE.md 节"
        );
    }

    #[test]
    fn 仅存在的文件被注入_空文件跳过() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("CLAUDE.md"), "  \n ").unwrap();
        assert_eq!(
            assemble_system_prompt(dir.path()).unwrap(),
            DEFAULT_SYSTEM_PROMPT.trim_end()
        );
        fs::write(dir.path().join("CLAUDE.md"), "偏好").unwrap();
        let prompt = assemble_system_prompt(dir.path()).unwrap();
        assert!(prompt.contains("## CLAUDE.md\n\n偏好"));
        assert!(!prompt.contains("AGENTS.md"));
    }

    #[test]
    fn 内容首尾空白被规整() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "\n\n  规约 A  \n\n").unwrap();
        let prompt = assemble_system_prompt(dir.path()).unwrap();
        assert!(prompt.contains("## AGENTS.md\n\n规约 A"));
        assert!(!prompt.contains("  规约 A  "));
    }
}
