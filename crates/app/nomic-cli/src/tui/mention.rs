//! `@mention`：语法、补全候选与发送时展开。
//!
//! mention 是输入草稿里的内联标记，支持两类：
//!
//! - `@skill:<name>`：引用一个已发现的 skill（名称即 skill 目录名）
//! - `@file:<path>`：引用一个文件（相对 cwd 或绝对路径）
//!
//! `@` 本身不触发任何动作；补全弹层只负责填好标记文本。消息提交（Enter
//! 发送）后，由 driver 在发送前调用 [`expand_mentions`] 把**有效**的标记
//! 展开为对应内容，与用户其余输入一起交给模型；找不到 skill / 文件不可读
//! 的标记**原样保留**，不阻断发送。
//!
//! 展开产出的块沿用既有契约：skill 用 [`nomic_skills::ActivatedSkill::prompt_tag`]
//! 的 `<active_skill>` 标签（与 `--skill`、`skill:<name>` 同一格式），文件用
//! `<file path="...">` 标签；chat 侧据此把展开块折叠为紧凑行展示。

use std::path::{Path, PathBuf};

use nomic_skills::SkillResolver;

/// skill mention 前缀。
pub(super) const SKILL_PREFIX: &str = "@skill:";
/// file mention 前缀。
pub(super) const FILE_PREFIX: &str = "@file:";

/// 文件 mention 展开的体积上限（避免误提及超大文件撑爆上下文）。
const MAX_FILE_MENTION_BYTES: u64 = 1 << 20;

/// 光标位于文本末尾、文本以 `@` 收尾且 `@` 后无空白时，返回该 mention
/// 片段（从 `@` 到末尾）。调用方再按片段内容决定是否弹出补全。
pub(super) fn mention_fragment(text: &str) -> Option<&str> {
    let at = text.rfind('@')?;
    if !is_mention_start(text, at) {
        return None;
    }
    let fragment = &text[at..];
    if fragment[1..].contains(|c: char| c.is_whitespace()) {
        return None;
    }
    Some(fragment)
}

/// 展开文本中的全部有效 mention；无效标记原样保留。
pub(super) fn expand_mentions(text: &str, skills: &SkillResolver, cwd: &Path) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if text.as_bytes()[index] == b'@'
            && is_mention_start(text, index)
            && let Some((expanded, consumed)) = expand_one(&text[index..], skills, cwd)
        {
            out.push_str(&expanded);
            index += consumed;
            continue;
        }
        let ch = text[index..]
            .chars()
            .next()
            .expect("index 始终落在 char 边界");
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

/// `@file:<prefix>` 的文件候选（相对 cwd 的路径列表，仅常规文件，按名排序）。
///
/// `prefix` 可能带目录部分（如 `src/ma`）：按最后一个 `/` 拆成目录与文件名
/// 前缀，只列出该目录下前缀匹配的文件；目录不存在或不可读时返回空。
pub(super) fn file_mention_candidates(prefix: &str, cwd: &Path) -> Vec<String> {
    let (dir_part, name_part) = match prefix.rfind('/') {
        Some(index) => (&prefix[..=index], &prefix[index + 1..]),
        None => ("", prefix),
    };
    let base = if dir_part.is_empty() {
        cwd.to_path_buf()
    } else {
        // join 对绝对路径（dir_part 以 `/` 开头）会直接返回该绝对路径
        cwd.join(dir_part)
    };
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(name_part) {
            let rel = if dir_part.is_empty() {
                name
            } else {
                format!("{dir_part}{name}")
            };
            out.push(rel);
        }
    }
    out.sort();
    out
}

/// 从 `<file path="...">` 标签头提取 path 属性（chat 折叠展示用）。
pub(super) fn file_block_path(block: &str) -> Option<&str> {
    let header = block.strip_prefix("<file ")?;
    let header = &header[..header.find('>')?];
    let needle = "path=\"";
    let start = header.find(needle)? + needle.len();
    let end = header[start..].find('"')? + start;
    Some(&header[start..end])
}

/// `@` 是否处于 mention 边界（串首或前导空白）。
fn is_mention_start(text: &str, at: usize) -> bool {
    at == 0
        || text[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)
}

/// 尝试从 `tail`（以 `@` 开头）展开一个 mention。
/// 返回 `(展开文本, 消耗的字节数)`；不是有效 mention 时返回 `None`。
fn expand_one(tail: &str, skills: &SkillResolver, cwd: &Path) -> Option<(String, usize)> {
    if let Some(name) = tail.strip_prefix(SKILL_PREFIX) {
        let name_len = skill_name_len(name);
        if name_len == 0 {
            return None;
        }
        let name = &name[..name_len];
        let skill = skills.activate(name).ok()?;
        let expanded = skill.prompt_tag();
        return Some((expanded, SKILL_PREFIX.len() + name_len));
    }
    if let Some(path) = tail.strip_prefix(FILE_PREFIX) {
        let path_len = path_len(path);
        if path_len == 0 {
            return None;
        }
        let path = &path[..path_len];
        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            cwd.join(path)
        };
        let metadata = std::fs::metadata(&resolved).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_MENTION_BYTES {
            return None;
        }
        let content = std::fs::read_to_string(&resolved).ok()?;
        let expanded = format!(
            "<file path=\"{}\">\n{}\n</file>",
            resolved.display(),
            content.trim_end()
        );
        return Some((expanded, FILE_PREFIX.len() + path_len));
    }
    None
}

/// `@skill:` 后 skill 名的字节长度：取合法名称字符（小写 ASCII 字母、
/// 数字、`-`、`_`）的最大前缀，自然在标点/空白处截断。
fn skill_name_len(name: &str) -> usize {
    name.chars()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_')
        .map(char::len_utf8)
        .sum()
}

/// `@file:` 后路径的字节长度：到首个空白为止（空格即结束 mention 值）。
fn path_len(path: &str) -> usize {
    path.find(|c: char| c.is_whitespace()).unwrap_or(path.len())
}

#[cfg(test)]
mod tests {
    use nomic_skills::{ProjectDiscovery, SkillRoot, SkillScope};

    use super::*;

    fn skill_resolver(root: &Path, skills: &[(&str, &str)]) -> SkillResolver {
        for (name, body) in skills {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).expect("mkdir skill");
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\ndescription: {name} desc\n---\n{body}"),
            )
            .expect("write skill");
        }
        SkillResolver::new(
            root,
            ProjectDiscovery::Roots(Vec::new()),
            vec![SkillRoot {
                path: root.to_path_buf(),
                scope: SkillScope::Project,
            }],
        )
        .expect("resolver")
    }

    // ── mention_fragment ────────────────────────────────────────────────────

    #[test]
    fn fragment_requires_mention_boundary_and_no_trailing_space() {
        assert_eq!(mention_fragment("用 @"), Some("@"));
        assert_eq!(mention_fragment("@skill:ju"), Some("@skill:ju"));
        assert_eq!(mention_fragment("看看 @file:src/ma"), Some("@file:src/ma"));
        assert_eq!(mention_fragment("没有 at 符号"), None);
        // `@` 后出现空白则视为普通文本，不再作为 mention
        assert_eq!(mention_fragment("@skill:ju "), None);
        // 前导非空白不构成 mention 边界（如邮箱）
        assert_eq!(mention_fragment("a@b"), None);
    }

    // ── expand_mentions：skill ──────────────────────────────────────────────

    #[test]
    fn expands_valid_skill_and_keeps_invalid_literal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resolver = skill_resolver(dir.path(), &[("rust-review", "Check unsafe code.")]);

        let text = "用 @skill:rust-review 审查，还有 @skill:missing 原样";
        let expanded = expand_mentions(text, &resolver, dir.path());

        assert!(
            expanded.contains("<active_skill name=\"rust-review\""),
            "{expanded}"
        );
        assert!(expanded.contains("Check unsafe code."), "{expanded}");
        assert!(expanded.contains("@skill:missing"), "{expanded}");
    }

    #[test]
    fn skill_name_stops_at_punctuation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resolver = skill_resolver(dir.path(), &[("rust-review", "body")]);

        let expanded = expand_mentions("@skill:rust-review，请审查", &resolver, dir.path());
        assert!(expanded.contains("body"), "{expanded}");
        assert!(expanded.contains("，请审查"), "{expanded}");
        assert!(!expanded.contains("rust-review，"), "{expanded}");
    }

    // ── expand_mentions：file ───────────────────────────────────────────────

    #[test]
    fn expands_valid_file_relative_to_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("notes.txt"), "文件内容\n第二行").expect("write file");

        let expanded = expand_mentions(
            "参考 @file:notes.txt 处理",
            &skill_resolver(dir.path(), &[]),
            dir.path(),
        );

        assert!(expanded.contains("<file path=\""), "{expanded}");
        assert!(expanded.contains("文件内容\n第二行"), "{expanded}");
        assert!(!expanded.contains("@file:notes.txt"), "{expanded}");
    }

    #[test]
    fn keeps_missing_or_unreadable_file_literal() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("subdir")).expect("mkdir");
        let resolver = skill_resolver(dir.path(), &[]);

        let expanded = expand_mentions(
            "@file:missing.txt 与 @file:subdir 都不展开",
            &resolver,
            dir.path(),
        );
        assert_eq!(
            expanded, "@file:missing.txt 与 @file:subdir 都不展开",
            "无效 mention 必须原样保留"
        );
    }

    // ── file_mention_candidates ─────────────────────────────────────────────

    #[test]
    fn lists_files_matching_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("README.md"), "").expect("write");
        std::fs::write(dir.path().join("src/main.rs"), "").expect("write");
        std::fs::write(dir.path().join("src/lib.rs"), "").expect("write");
        std::fs::create_dir_all(dir.path().join("src/nested")).expect("mkdir dir");

        assert_eq!(
            file_mention_candidates("", dir.path()),
            vec!["README.md".to_string()]
        );
        assert_eq!(
            file_mention_candidates("src/ma", dir.path()),
            vec!["src/main.rs".to_string()]
        );
        // 目录不列入候选
        assert_eq!(
            file_mention_candidates("src/", dir.path()),
            vec!["src/lib.rs".to_string(), "src/main.rs".to_string(),]
        );
        assert!(file_mention_candidates("src/miss", dir.path()).is_empty());
    }

    #[test]
    fn file_block_path_extracts_attribute() {
        assert_eq!(
            file_block_path("<file path=\"/tmp/a.txt\">\nbody\n</file>"),
            Some("/tmp/a.txt")
        );
        assert_eq!(file_block_path("not a file block"), None);
    }
}
