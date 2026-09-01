//! write/edit 共用的内部 URI 写入闸（ADR-0040 §8.1）。
//!
//! 语义分层：
//! - 已注册 scheme 且资源不可变 → 只读错误（协议专用变更工具是唯一合法入口）；
//! - 已注册 scheme 且可变 → 返回 [`WritableTarget`]，调用方经 router 写入；
//! - 层级式 URI 但 scheme 未注册 → UnknownScheme 错误（不静默落到文件系统）；
//! - 其余输入 → `None`，走文件系统路径分支。
//!
//! 尾挂选择器（`:N-M` / `:raw`）不是合法的写入目标：先剥离再报错。

use nomic_core::ToolError;
use nomic_uri::{UriResource, UriRouter, parse::hierarchical_scheme, split_uri_selector};

/// 通过闸的可写 URI 目标。
pub struct WritableTarget<'a> {
    /// 会话路由器（写入分发用）
    pub router: &'a UriRouter,
    /// 剥掉选择器的干净 URI
    pub href: String,
    /// 闸内已解析的资源（edit 的读-改-写复用，避免二次 resolve）
    pub resource: UriResource,
}

/// 写入闸。`Ok(None)` = 非 URI 输入；`Ok(Some)` = 可写；`Err` = 已拦截。
pub async fn guard_writable<'a>(
    router: &'a UriRouter,
    target: &str,
) -> Result<Option<WritableTarget<'a>>, ToolError> {
    if !router.can_resolve(target) {
        if hierarchical_scheme(target).is_some() {
            // 层级式但未注册：借 router 的 UnknownScheme 错误（带可用列表）
            let error = router
                .resolve(target)
                .await
                .expect_err("unregistered scheme must fail");
            return Err(ToolError::new(error.to_string()));
        }
        return Ok(None);
    }
    let (clean, sel) = split_uri_selector(target);
    if sel.is_some() {
        return Err(ToolError::new(format!(
            "Line selectors (:N-M, :raw) are not valid write targets: {target}. \
             Remove the selector and retry."
        )));
    }
    let resource = router
        .resolve(&clean)
        .await
        .map_err(|error| ToolError::new(error.to_string()))?;
    if resource.is_immutable() {
        return Err(ToolError::new(format!(
            "{clean} is a read-only resource; do not modify it. \
             Use read to inspect, or the protocol's dedicated mutation tool if one exists."
        )));
    }
    Ok(Some(WritableTarget {
        router,
        href: clean,
        resource,
    }))
}
