//! 文件变更队列：同一路径的写/编辑操作串行化（parallel 工具执行下防写冲突）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tokio::sync::OwnedMutexGuard;

fn queue() -> &'static Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>> {
    static QUEUE: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 获取某路径的变更锁（guard 持有点内同路径操作互斥）。
pub async fn lock_path(path: &Path) -> OwnedMutexGuard<()> {
    let lock: Arc<tokio::sync::Mutex<()>> = {
        let mut map: MutexGuard<'_, HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>> = queue()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    lock.lock_owned().await
}
