//! 工具基准目录（workspace 严格归属）：session 内工具的相对路径以其
//! workspace 路径解析；未设置时退回进程 cwd（由 OS 隐式解析，行为同现状）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// 工具共享的基准目录句柄：clone 即共享同一份状态。
///
/// 交互端（TUI）在 resume/new 切换 session 的 workspace 时经 [`BaseDir::set`]
/// 原地更新，已构建的工具在下一次执行时读到新基准；一次性前端（print）与
/// per-session 运行时（web）用固定值构建即可，无需再写。
#[derive(Clone, Debug, Default)]
pub struct BaseDir(Arc<RwLock<Option<PathBuf>>>);

impl BaseDir {
    /// 以固定初始值创建（`None` = 进程 cwd，行为同未设置）。
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self(Arc::new(RwLock::new(dir)))
    }

    /// 更新基准目录（切换到另一个 workspace）。
    pub fn set(&self, dir: PathBuf) {
        tracing::debug!(dir = %dir.display(), "base dir updated");
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(dir);
    }

    /// 读取当前基准快照；`None` 表示退回进程 cwd。
    ///
    /// 锁中毒时取回内部值：基准读取是纯数据访问，不应因别的线程中毒而失败。
    pub fn snapshot(&self) -> Option<PathBuf> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl From<Option<PathBuf>> for BaseDir {
    fn from(dir: Option<PathBuf>) -> Self {
        Self::new(dir)
    }
}

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
