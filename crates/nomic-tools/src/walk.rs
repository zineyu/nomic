//! grep/find 共用的目录遍历：ripgrep/fd 同款 `ignore` crate，
//! 默认遵守 .gitignore/.ignore 并跳过隐藏文件；`include_hidden` 放开隐藏文件。
//!
//! `require_git(false)`：即使目录不在 git 仓库内也遵守 .gitignore
//! （比 rg 默认更宽松，对 agent 探索临时目录更友好）。

use std::path::Path;

/// 遍历 root，产出遵守忽略规则的所有条目（含目录本身之外的条目）。
///
/// 顺序不做保证，调用方需要确定性输出时自行排序。
pub fn walk(root: &Path, include_hidden: bool) -> Vec<ignore::DirEntry> {
    ignore::WalkBuilder::new(root)
        .hidden(!include_hidden)
        .require_git(false)
        .build()
        .filter_map(Result::ok)
        .skip(1) // 跳过 root 自身
        .collect()
}
