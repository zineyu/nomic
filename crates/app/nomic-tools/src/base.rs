//! 工具基准目录（workspace 严格归属）：session 内工具的相对路径以其
//! workspace 路径解析；未设置时退回进程 cwd（由 OS 隐式解析，行为同现状）。
//!
//! 句柄本体在 [`nomic_uri::WorkspaceRoot`]（`local://` 协议 handler 与各
//! 文件工具共享同一份状态）；本模块保留 `BaseDir` 别名与路径解析助手。

use std::path::{Path, PathBuf};

/// 工具共享的基准目录句柄（[`nomic_uri::WorkspaceRoot`] 的兼容别名）。
pub type BaseDir = nomic_uri::WorkspaceRoot;

/// 相对路径按基准目录解析；绝对路径原样返回。
pub fn resolve(base: Option<&Path>, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match base {
        Some(base) => base.join(path),
        None => path.to_path_buf(),
    }
}

/// 可选根目录参数（grep/find 的 `path` 缺省为搜索根）按基准目录解析：
/// 缺省时基准即搜索根（无基准则为 `.`，进程 cwd）。
pub fn resolve_root(base: Option<&Path>, path: Option<&str>) -> PathBuf {
    match path {
        Some(path) => resolve(base, path),
        None => base.map_or_else(|| PathBuf::from("."), Path::to_path_buf),
    }
}
