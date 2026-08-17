//! 前端静态产物内嵌：编译期把 `web/dist`（Vite build 输出）打包进二进制
//! （rust-embed），`--web` 直接伺服内嵌资源，SPA 未命中路径回退 `index.html`。
//!
//! 构建 nomic 前必须先在 `web/` 下 `npm run build`（`devenv.nix` 的
//! `web-build`/`check` 已保证顺序）；产物缺失时此处编译报错——这是有意的
//! 耦合：前端与后端强制同版本交付，发行包不再单独携带 `web/dist`。

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// `web/dist` 构建产物（相对 nomic-cli crate 根，即仓库 `web/dist`）。
#[derive(RustEmbed)]
#[folder = "../../../web/dist"]
struct WebAssets;

/// 伺服内嵌前端资源：精确匹配路径，未命中回退 `index.html`（SPA）。
///
/// 挂在 `/api/*` 之外的路由 fallback 上，因此不需要再拦截 API 路径；
/// 缺失场景只在产物被外部清掉时兜底提示（正常编译期已保证存在）。
pub fn serve(path: &str) -> Response {
    let normalized = path.trim_start_matches('/');
    let file = WebAssets::get(normalized).or_else(|| WebAssets::get("index.html"));
    match file {
        Some(file) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, file.metadata.mimetype())
            .body(Body::from(file.data.into_owned()))
            .expect("内嵌资源响应构造失败"),
        None => (
            StatusCode::NOT_FOUND,
            "前端产物内嵌缺失：构建 nomic 前先在 web/ 下运行 `npm run build`",
        )
            .into_response(),
    }
}
