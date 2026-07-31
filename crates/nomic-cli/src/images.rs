//! 图片附件加载：本地图片文件 → base64 内联的 [`ImageContent`] 内容块。
//!
//! print 模式的 `--image` 与 TUI 的 `/image` 共用。MIME 由扩展名初判、
//! 魔数复核（两者不一致视为损坏/伪装文件，拒绝加载），避免把非图片字节
//! 原样发给 provider。

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use nomic_ai::ImageContent;

/// 单张图片的原始字节上限（16 MiB）。
///
/// provider 侧各有自己的上限（Anthropic 按 base64 后 5MB 计），这里只挡
/// 明显异常的输入（误传视频/磁盘镜像等），精确尺寸适配由 provider 报错兜底。
const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// 支持的图片格式：扩展名 → MIME。
const EXTENSION_MIME: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// 加载图片文件为内联内容块（同步读：本地小文件，含上限保护）。
pub fn load_image(path: &Path) -> Result<ImageContent> {
    let declared =
        extension_mime(path).with_context(|| format!("不支持的图片类型：{}", path.display()))?;
    let data = std::fs::read(path).with_context(|| format!("读取图片失败：{}", path.display()))?;
    if data.len() > MAX_IMAGE_BYTES {
        bail!(
            "图片超过大小上限（{} MiB）：{}",
            MAX_IMAGE_BYTES / 1024 / 1024,
            path.display()
        );
    }
    let detected =
        sniff_mime(&data).with_context(|| format!("无法识别的图片内容：{}", path.display()))?;
    if detected != declared {
        bail!(
            "图片内容与扩展名不符（扩展名 {declared}，内容 {detected}）：{}",
            path.display()
        );
    }
    Ok(ImageContent {
        data: STANDARD.encode(&data),
        mime_type: declared.to_string(),
    })
}

/// 按扩展名初判 MIME（大小写不敏感）。
fn extension_mime(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    EXTENSION_MIME
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, mime)| *mime)
}

/// 按魔数复核 MIME。
fn sniff_mime(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if data.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn write_temp(extension: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(format!("img.{extension}"));
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(bytes).expect("write");
        (dir, path)
    }

    /// 最小合法 PNG 头 + 填充。
    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR";

    #[test]
    fn loads_png_with_correct_mime() {
        let (_dir, path) = write_temp("png", PNG_BYTES);
        let image = load_image(&path).expect("load");
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.data, STANDARD.encode(PNG_BYTES));
    }

    #[test]
    fn extension_is_case_insensitive() {
        let (_dir, path) = write_temp("PNG", PNG_BYTES);
        assert_eq!(load_image(&path).expect("load").mime_type, "image/png");
    }

    #[test]
    fn rejects_unsupported_extension() {
        let (_dir, path) = write_temp("bmp", PNG_BYTES);
        let error = load_image(&path).expect_err("bmp unsupported");
        assert!(error.to_string().contains("不支持的图片类型"));
    }

    #[test]
    fn rejects_extension_content_mismatch() {
        // JPEG 魔数伪装成 .png
        let (_dir, path) = write_temp("png", b"\xff\xd8\xff\xe0JFIF");
        let error = load_image(&path).expect_err("mismatch");
        assert!(error.to_string().contains("扩展名"));
    }

    #[test]
    fn rejects_non_image_content() {
        let (_dir, path) = write_temp("png", b"not an image at all");
        let error = load_image(&path).expect_err("not an image");
        assert!(error.to_string().contains("无法识别"));
    }

    #[test]
    fn rejects_missing_file() {
        let error = load_image(Path::new("/nonexistent/pic.png")).expect_err("missing");
        assert!(error.to_string().contains("读取图片失败"));
    }

    #[test]
    fn rejects_oversized_image() {
        let mut bytes = PNG_BYTES.to_vec();
        bytes.resize(MAX_IMAGE_BYTES + 1, 0);
        let (_dir, path) = write_temp("png", &bytes);
        let error = load_image(&path).expect_err("oversized");
        assert!(error.to_string().contains("大小上限"));
    }
}
