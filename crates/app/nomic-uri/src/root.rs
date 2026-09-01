//! 会话 workspace 根句柄：内部 URI 协议（`local://`）与文件工具共用的
//! 基准目录。clone 即共享同一份状态；交互端切换 session workspace 时经
//! [`WorkspaceRoot::set`] 原地更新，下一次工具执行即用新基准。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// 共享的 workspace 根目录句柄。
///
/// 未设置（`None`）时调用方退回进程 cwd（行为同未配置）。
#[derive(Clone, Debug, Default)]
pub struct WorkspaceRoot(Arc<RwLock<Option<PathBuf>>>);

impl WorkspaceRoot {
    /// 以固定初始值创建（`None` = 进程 cwd）。
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self(Arc::new(RwLock::new(dir)))
    }

    /// 更新根目录（切换到另一个 workspace）。
    pub fn set(&self, dir: PathBuf) {
        tracing::debug!(dir = %dir.display(), "workspace root updated");
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(dir);
    }

    /// 读取当前根快照；`None` 表示退回进程 cwd。
    ///
    /// 锁中毒时取回内部值：根读取是纯数据访问，不应因别的线程中毒而失败。
    pub fn snapshot(&self) -> Option<PathBuf> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl From<Option<PathBuf>> for WorkspaceRoot {
    fn from(dir: Option<PathBuf>) -> Self {
        Self::new(dir)
    }
}
