//! 剪贴板与图片暂存：bracketed paste、Ctrl+V 粘贴、`/copy` 复制、
//! `/image` 与 `--image` 附件暂存。

use crate::tui::app::App;

/// 加载图片并暂存为附件（`/image` 与粘贴图片路径共用）。
pub(in crate::tui) fn attach_image(app: &mut App, path: &std::path::Path) {
    match crate::images::load_image(path) {
        Ok(image) => {
            let name = attachment_name(path);
            let count = app.input_mut().stage_image(name.clone(), image);
            app.chat_mut().push_system(format!(
                "已附加图片 {name}（共 {count} 张，随下一条消息发送）。"
            ));
        }
        Err(error) => app.warn(format!("附加图片失败：{error:#}")),
    }
}

/// 粘贴整段文本（bracketed paste）：形似图片路径的转为附件，其余原样插入输入框。
///
/// 「形似」只按扩展名初判（裸路径 / `file://` URI / 引号包裹均可），
/// 能否加载由 [`crate::images::load_image`] 复核；多行或普通文本走插入。
pub(in crate::tui) fn handle_paste(app: &mut App, text: &str) {
    if let Some(path) = paste_image_path(text) {
        attach_image(app, &path);
    } else {
        app.paste_text(text);
    }
}

/// 从粘贴文本中识别图片路径：单行、支持 file:// URI（含百分号解码）与引号包裹。
fn paste_image_path(text: &str) -> Option<std::path::PathBuf> {
    let text = text.trim();
    if text.is_empty() || text.contains(['\n', '\t']) {
        return None;
    }
    let candidate = if let Some(uri) = text.strip_prefix("file://") {
        // file:///abs/path 与 file://localhost/abs/path
        let uri = uri.strip_prefix("localhost").unwrap_or(uri);
        percent_decode(uri)?
    } else {
        text.trim_matches(['\'', '"']).to_string()
    };
    let path = std::path::PathBuf::from(candidate);
    if crate::images::is_supported_image_path(&path) {
        Some(path)
    } else {
        None
    }
}

/// 百分号解码（file:// URI 中的 %20 等）；非法序列或结果非 UTF-8 返回 None。
fn percent_decode(input: &str) -> Option<String> {
    if !input.contains('%') {
        return Some(input.to_string());
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = input.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Ctrl+V 粘贴剪贴板：图片暂存为附件，文本插入输入框。
///
/// 剪贴板读取可能阻塞在 X11/Wayland 往返上，放 `spawn_blocking` 中执行；
/// 期间事件循环不阻塞，结果返回前界面照常重绘。
pub(in crate::tui) async fn paste_clipboard(app: &mut App) {
    match tokio::task::spawn_blocking(crate::clipboard::read).await {
        Ok(Ok(Some(crate::clipboard::ClipboardContent::Image(image)))) => {
            let name = format!("clipboard-{}.png", nomic_ai::now_millis());
            let count = app.input_mut().stage_image(name.clone(), image);
            app.chat_mut().push_system(format!(
                "已粘贴图片 {name}（共 {count} 张，随下一条消息发送）。"
            ));
        }
        Ok(Ok(Some(crate::clipboard::ClipboardContent::Text(text)))) => app.paste_text(&text),
        Ok(Ok(None)) => app.warn("剪贴板中没有图片或文本"),
        Ok(Err(error)) => app.warn(format!("粘贴失败：{error:#}")),
        Err(join) => app.warn(format!("粘贴失败：{join}")),
    }
}

/// `/copy`：把文本写入系统剪贴板。
///
/// 与粘贴同理，写入可能阻塞在 X11/Wayland 往返上，放 `spawn_blocking` 中执行。
pub(in crate::tui) async fn copy_to_clipboard(app: &mut App, text: String) {
    let chars = text.chars().count();
    match tokio::task::spawn_blocking(move || crate::clipboard::write_text(&text)).await {
        Ok(Ok(())) => app
            .chat_mut()
            .push_system(format!("已复制到剪贴板（{chars} 字）。")),
        Ok(Err(error)) => app.warn(format!("复制失败：{error:#}")),
        Err(join) => app.warn(format!("复制失败：{join}")),
    }
}
/// 附件展示名：取文件名，缺失时回退完整路径。
fn attachment_name(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// 把启动参数 `--image` 载入为暂存附件（失败以系统条目提示，不中止启动）。
pub(in crate::tui) fn stage_cli_images(app: &mut App, paths: &[std::path::PathBuf]) {
    for path in paths {
        match crate::images::load_image(path) {
            Ok(image) => {
                let name = attachment_name(path);
                let count = app.input_mut().stage_image(name.clone(), image);
                app.chat_mut().push_system(format!(
                    "已附加图片 {name}（共 {count} 张，随下一条消息发送）。"
                ));
            }
            Err(error) => app
                .chat_mut()
                .push_system(format!("加载图片附件失败：{error:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{paste_image_path, percent_decode};

    #[test]
    fn paste_recognizes_plain_image_path() {
        assert_eq!(
            paste_image_path("/tmp/pic.png"),
            Some(PathBuf::from("/tmp/pic.png"))
        );
        // 相对路径与大写扩展名
        assert_eq!(
            paste_image_path("shots/UPPER.PNG"),
            Some(PathBuf::from("shots/UPPER.PNG"))
        );
    }

    #[test]
    fn paste_recognizes_file_uri_and_decodes() {
        assert_eq!(
            paste_image_path("file:///tmp/my%20pics/a%20b.png"),
            Some(PathBuf::from("/tmp/my pics/a b.png"))
        );
        assert_eq!(
            paste_image_path("file://localhost/tmp/pic.webp"),
            Some(PathBuf::from("/tmp/pic.webp"))
        );
    }

    #[test]
    fn paste_recognizes_quoted_path() {
        assert_eq!(
            paste_image_path("'/tmp/with space/pic.jpg'"),
            Some(PathBuf::from("/tmp/with space/pic.jpg"))
        );
        assert_eq!(
            paste_image_path("\"/tmp/pic.gif\""),
            Some(PathBuf::from("/tmp/pic.gif"))
        );
    }

    #[test]
    fn paste_ignores_non_image_text() {
        assert_eq!(paste_image_path("hello world"), None);
        assert_eq!(paste_image_path("/tmp/notes.txt"), None);
        assert_eq!(paste_image_path("multi\nline /tmp/pic.png"), None);
        assert_eq!(paste_image_path(""), None);
        // 非法百分号序列不视为路径
        assert_eq!(paste_image_path("file:///tmp/%zz.png"), None);
    }

    #[test]
    fn percent_decode_roundtrip() {
        assert_eq!(
            percent_decode("/a%20b/%E4%B8%AD.png"),
            Some("/a b/中.png".to_string())
        );
        assert_eq!(percent_decode("no-escape"), Some("no-escape".to_string()));
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%xy"), None);
    }
}
