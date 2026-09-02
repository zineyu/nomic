//! 内部 URI 与文件工具的桥接（ADR-0040 §8.1，ADR-0042 VFS 化）：
//! write/edit 的写入闸 + grep/bash 的 source_path 对齐助手。
//!
//! 语义分层：
//! - 已挂载 scheme 且资源不可变 → 只读错误（协议专用变更工具是唯一合法入口）；
//! - 已挂载 scheme 且可变 → 返回 [`WritableTarget`]，调用方经 router 写入；
//! - 层级式 URI 但 scheme 未挂载 → UnknownScheme 错误（不静默落到文件系统）；
//! - 其余输入 → `None`，走文件系统路径分支。
//!
//! 元信息检查一律走 [`VfsRouter::stat`]，不整读内容。尾挂选择器
//!（`:N-M` / `:raw`）不是合法的写入目标：先剥离再报错。

use std::path::PathBuf;

use nomic_core::ToolError;
use nomic_vfs::{VfsMetadata, VfsRouter, parse::hierarchical_scheme, split_uri_selector};

/// 通过闸的可写 URI 目标。
pub struct WritableTarget<'a> {
    /// 会话路由器（写入分发用）
    pub router: &'a VfsRouter,
    /// 剥掉选择器的干净 URI
    pub href: String,
    /// 闸内已 stat 的元数据；`None` = 资源尚不存在
    ///（write 可新建，edit 须报错）
    pub meta: Option<VfsMetadata>,
}

/// 写入闸。`Ok(None)` = 非 URI 输入；`Ok(Some)` = 可写；`Err` = 已拦截。
pub async fn guard_writable<'a>(
    router: &'a VfsRouter,
    target: &str,
) -> Result<Option<WritableTarget<'a>>, ToolError> {
    if !router.can_resolve(target) {
        if hierarchical_scheme(target).is_some() {
            // 层级式但未挂载：借 router 的 UnknownScheme 错误（带可用列表）
            let error = router
                .read(target)
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
    let meta = match router.stat(&clean).await {
        Ok(meta) => Some(meta),
        Err(error) => {
            // 可写协议允许写入不存在的目标（write 新建）；只读协议照常报错
            let writable = nomic_vfs::parse_internal_uri(&clean)
                .ok()
                .and_then(|url| router.vfs(&url.scheme))
                .is_some_and(|vfs| vfs.capabilities().writable);
            if writable {
                None
            } else {
                return Err(ToolError::new(error.to_string()));
            }
        }
    };
    if meta.as_ref().is_some_and(VfsMetadata::is_immutable) {
        return Err(ToolError::new(format!(
            "{clean} is a read-only resource; do not modify it. \
             Use read to inspect, or the protocol's dedicated mutation tool if one exists."
        )));
    }
    Ok(Some(WritableTarget {
        router,
        href: clean,
        meta,
    }))
}

/// 把 URI 输入对齐到底层文件系统路径（grep 搜索根、bash `cd` 目标）。
///
/// - 未挂载 / 非 URI 输入 → `Ok(None)`（调用方走原路径解析）；
/// - 已挂载 → stat（纯元数据，不读内容）后返回 `source_path`——目录化
///   不变量下恒有值（ADR-0043）；
/// - 尾挂选择器对搜索/执行无语义 → 明确报错而非静默忽略。
pub async fn vfs_source_path(
    router: &VfsRouter,
    input: &str,
    tool: &str,
) -> Result<Option<PathBuf>, ToolError> {
    if !router.can_resolve(input) {
        return Ok(None);
    }
    let (clean, sel) = split_uri_selector(input);
    if sel.is_some() {
        return Err(ToolError::new(format!(
            "Line selectors (:N-M, :raw) have no meaning for {tool}: {input}. \
             Remove the selector and retry."
        )));
    }
    let meta = router
        .stat(&clean)
        .await
        .map_err(|error| ToolError::new(error.to_string()))?;
    Ok(Some(meta.source_path))
}
