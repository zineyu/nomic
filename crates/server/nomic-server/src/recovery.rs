//! 启动恢复：崩溃残留的执行中会话回到待处理，待处理工作重驱。
//!
//! 存储是唯一事实源：上次进程死亡时被 worker 持有的会话停留在
//! 运行中，重启后没有 worker 持有它。恢复把这类会话转回待处理，
//! 与本来待处理的会话一起交给调度器重驱；worker 从最后 checkpoint
//! 继续（在途工具调用由 worker 闭合）。静止会话没有待处理工作，
//! 不被唤醒。

use nomic_core::{SessionId, SessionState, Storage, StorageError};

/// 把崩溃残留的执行中会话转回待处理，返回需要重驱的会话 id。
///
/// 返回顺序稳定（创建先后），入队顺序与此一致。
///
/// # Errors
///
/// 存储读取或写入失败时返回 [`StorageError`]：启动恢复无法确定
/// 待处理工作时，server 不得在未知状态下开始服务。
pub async fn recoverable_sessions<S: Storage>(storage: &S) -> Result<Vec<SessionId>, StorageError> {
    for session in storage.session_ids_in_state(SessionState::Running).await? {
        storage.set_state(session, SessionState::Pending).await?;
    }
    storage.session_ids_in_state(SessionState::Pending).await
}
