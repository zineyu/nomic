//! 剪贴板读取（TUI `Ctrl+V` 粘贴）：优先取图片，其次文本。
//!
//! 平台支持：macOS / Windows / X11 / Wayland（data-control 协议，优先于 X11）。
//! 无桌面环境（SSH、纯控制台）时 `Clipboard::new` 失败，调用方降级为提示。
//! 读取可能阻塞在 X11/Wayland 往返上，调用方应放在 `spawn_blocking` 中。

use anyhow::{Context as _, Result};
use nomic_ai::ImageContent;

/// 剪贴板内容。
pub enum ClipboardContent {
    /// 图片（已编码为 PNG 内容块）
    Image(ImageContent),
    /// 文本
    Text(String),
}

/// 读取剪贴板：有图片返回图片，否则有文本返回文本，都没有返回 `None`。
pub fn read() -> Result<Option<ClipboardContent>> {
    let mut clipboard = arboard::Clipboard::new().context("剪贴板不可用（无桌面环境？）")?;
    if let Ok(image) = clipboard.get_image() {
        let width = u32::try_from(image.width).context("剪贴板图片尺寸异常")?;
        let height = u32::try_from(image.height).context("剪贴板图片尺寸异常")?;
        return crate::images::image_from_rgba(width, height, &image.bytes)
            .map(|content| Some(ClipboardContent::Image(content)));
    }
    match clipboard.get_text() {
        Ok(text) if !text.trim().is_empty() => Ok(Some(ClipboardContent::Text(text))),
        _ => Ok(None),
    }
}
