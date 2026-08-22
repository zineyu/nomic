//! AGENTS.md 上下文文件发现（对齐 pi-coding-agent 的语义）。
//!
//! 从当前目录一路向上走到文件系统根，收集沿途每个目录的 `AGENTS.md`，
//! 按**根到叶**的顺序返回 —— 越靠近 cwd 的指令越靠后，可细化上层指令。
//! 不在 git 仓库根处停住：父目录的 AGENTS.md（如工作区级约定）同样是
//! 指令层级的一部分。

use std::path::{Path, PathBuf};

/// 一份已加载的 AGENTS.md：绝对路径 + 全文内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub path: PathBuf,
    pub content: String,
}

/// 从 `cwd` 向上发现全部 AGENTS.md，按根到叶排序。
///
/// 缺失与纯空白文件跳过；存在但不可读/非 UTF-8 的文件告警后跳过，
/// 不阻断启动。
pub fn discover_agents_files(cwd: &Path) -> Vec<ContextFile> {
    let mut files: Vec<ContextFile> = cwd
        .ancestors()
        .filter_map(|dir| {
            let path = dir.join("AGENTS.md");
            read_context_file(&path)
        })
        .collect();
    files.reverse();
    tracing::debug!(count = files.len(), "context files discovered");
    files
}

/// 读取单个 AGENTS.md：不存在/空白 → `None`；读取失败 → 告警并跳过。
fn read_context_file(path: &Path) -> Option<ContextFile> {
    if !path.is_file() {
        return None;
    }
    match std::fs::read_to_string(path) {
        Ok(content) if content.trim().is_empty() => None,
        Ok(content) => Some(ContextFile {
            path: path.to_path_buf(),
            content,
        }),
        Err(error) => {
            eprintln!(
                "\x1b[33m⚠ 读取 {} 失败，已跳过：{error}\x1b[0m",
                path.display()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// 目录树：root/{AGENTS.md} → parent/{AGENTS.md} → leaf/{AGENTS.md}，
    /// 另有 sibling/{AGENTS.md} 不在 leaf 的祖先链上。
    struct Tree {
        _root: tempfile::TempDir,
        parent: PathBuf,
        leaf: PathBuf,
        sibling: PathBuf,
    }

    fn tree() -> Tree {
        let root = tempfile::tempdir().expect("tempdir");
        let parent = root.path().join("parent");
        let leaf = parent.join("leaf");
        let sibling = root.path().join("sibling");
        fs::create_dir_all(&leaf).expect("mkdir leaf");
        fs::create_dir_all(&sibling).expect("mkdir sibling");
        fs::write(root.path().join("AGENTS.md"), "root rules").expect("write root");
        fs::write(parent.join("AGENTS.md"), "parent rules").expect("write parent");
        fs::write(leaf.join("AGENTS.md"), "leaf rules").expect("write leaf");
        fs::write(sibling.join("AGENTS.md"), "sibling rules").expect("write sibling");
        Tree {
            _root: root,
            parent,
            leaf,
            sibling,
        }
    }

    #[test]
    fn discovers_ancestors_root_to_leaf() {
        let tree = tree();
        let files = discover_agents_files(&tree.leaf);
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        // 根到叶：越近的指令越靠后
        assert_eq!(contents, ["root rules", "parent rules", "leaf rules"]);
        // 路径与内容一一对应
        assert!(files[0].path.ends_with("AGENTS.md"));
        assert_eq!(files[2].path, tree.leaf.join("AGENTS.md"));
    }

    #[test]
    fn skips_directories_outside_ancestor_chain() {
        let tree = tree();
        let files = discover_agents_files(&tree.leaf);
        assert!(
            files.iter().all(|f| !f.path.starts_with(&tree.sibling)),
            "sibling 不在祖先链上，不应加载"
        );
        // 从 parent 出发则 leaf 也不应出现
        let files = discover_agents_files(&tree.parent);
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        assert_eq!(contents, ["root rules", "parent rules"]);
    }

    #[test]
    fn skips_missing_and_blank_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let leaf = root.path().join("leaf");
        fs::create_dir_all(&leaf).expect("mkdir");
        fs::write(root.path().join("AGENTS.md"), "   \n\t  ").expect("write blank");

        // 根为空白、leaf 缺失：都不产生 ContextFile
        let files = discover_agents_files(&leaf);
        assert!(
            files.iter().all(|f| !f.path.starts_with(root.path())),
            "空白与缺失文件应跳过，得到 {files:?}"
        );

        // 写入真实内容后正常加载
        fs::write(leaf.join("AGENTS.md"), "leaf rules").expect("write leaf");
        let files = discover_agents_files(&leaf);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].content, "leaf rules");
    }

    #[test]
    fn skips_non_utf8_file_without_error() {
        let root = tempfile::tempdir().expect("tempdir");
        fs::write(root.path().join("AGENTS.md"), [0xff, 0xfe, 0x00]).expect("write bytes");
        // read_to_string 失败 → 告警并跳过，不阻断发现
        assert!(discover_agents_files(root.path()).is_empty());
    }
}
