//! 内置挂载声明：一个 URI scheme 对应一个 [`Mount`](crate::Mount)
//! （ADR-0042，ADR-0043 目录化挂载）。
//!
//! - [`skill::SkillMount`]：`skill://`，只读提示词资产（复用 `SkillResolver`）
//! - [`local::LocalMount`]：`local://`，可写的会话 workspace 视图
//! - [`nix::NixMount`]：`nix://`，可写的 workspace nix 环境定义（ADR-0041）
//!
//! 声明经 [`DirMount`](crate::DirMount) 适配为完整 VFS（类型别名
//! [`SkillVfs`] / [`LocalVfs`] / [`NixVfs`]）。后续协议
//! （artifact/history/agent，ADR-0040 §协议目录）各自新增一个挂载
//! 声明并挂载进 router，无需改动工具层。

pub mod local;
pub mod nix;
pub mod skill;

pub use local::{LocalMount, LocalVfs};
pub use nix::{NixMount, NixVfs};
pub use skill::{SkillMount, SkillVfs};

use std::path::Path;

use crate::vfs::{ContentType, VfsEntry, VfsKind};

/// 目录清单 / 补全的条目数上限。
pub(crate) const MAX_LISTING_ENTRIES: usize = 1000;

/// 读取目录的类型化条目（有界：达到 `cap` 即截断）。
pub(crate) async fn dir_entries(path: &Path, cap: usize) -> std::io::Result<Vec<VfsEntry>> {
    let mut entries = Vec::new();
    let mut read_dir = tokio::fs::read_dir(path).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        if entries.len() >= cap {
            break;
        }
        let kind = if entry.file_type().await?.is_dir() {
            VfsKind::Directory
        } else {
            VfsKind::File
        };
        entries.push(VfsEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind,
        });
    }
    Ok(entries)
}

/// 按扩展名推断内容类别。
pub(crate) fn content_type_for(path: &Path) -> ContentType {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md" | "markdown") => ContentType::Markdown,
        Some("json") => ContentType::Json,
        _ => ContentType::Plain,
    }
}
