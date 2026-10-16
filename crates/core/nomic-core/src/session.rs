//! 会话与执行状态领域类型。
//!
//! 会话是树形结构中的一个节点：可从任意消息分叉，父子写时复制、
//! 相互独立。执行状态驱动调度重驱：待处理 → 运行中 → 静止。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SessionId};

/// 会话的父节点引用：分叉来源会话与分叉点消息。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParentRef {
    /// 父会话 id。
    pub session: SessionId,
    /// 分叉点：新会话复制父会话截至该消息（含）的前缀，此后继续。
    pub forked_at: MessageId,
}

/// 会话执行状态（调度重驱的驱动状态）。
///
/// 存储是唯一事实源；有待处理工作的会话处于 `Pending`，
/// 被调度器分发给 worker 后进入 `Running`，worker 执行到任务
/// 静止后释放为 `Idle`。worker / 进程死亡后状态回到 `Pending`，
/// 重驱从最后 checkpoint 继续。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// 待处理：有待处理工作，等待调度器分发。
    Pending,
    /// 运行中：已被某 worker 持有执行。
    Running,
    /// 静止：无待处理工作。
    Idle,
}

/// 非法的状态转换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("illegal session state transition: {from:?} via {op}")]
pub struct StateTransitionError {
    /// 转换前的状态。
    from: SessionState,
    /// 尝试的操作。
    op: &'static str,
}

impl SessionState {
    /// 调度器取走待处理会话：待处理 → 运行中。
    ///
    /// 静止会话没有待处理工作，必须经受理重新进入待处理后才可被
    /// 分发；运行中的会话不可被二次分发（同一时刻至多一个 worker）。
    pub const fn dispatch(self) -> Result<Self, StateTransitionError> {
        match self {
            Self::Pending => Ok(Self::Running),
            Self::Running | Self::Idle => Err(StateTransitionError {
                from: self,
                op: "dispatch",
            }),
        }
    }

    /// worker / 进程死亡，会话回到待处理队列等待重驱。
    pub const fn suspend(self) -> Result<Self, StateTransitionError> {
        match self {
            Self::Running => Ok(Self::Pending),
            Self::Pending | Self::Idle => Err(StateTransitionError {
                from: self,
                op: "suspend",
            }),
        }
    }

    /// 任务执行到静止，worker 释放会话。
    pub const fn quiesce(self) -> Result<Self, StateTransitionError> {
        match self {
            Self::Running => Ok(Self::Idle),
            Self::Pending | Self::Idle => Err(StateTransitionError {
                from: self,
                op: "quiesce",
            }),
        }
    }
}

/// 会话：与 agent 的一次连续对话，会话树中的一个节点。
///
/// 字段私有，只能经 [`crate::SessionTree`] 的构造路径创建，
/// 保证父子引用与状态合法。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    id: SessionId,
    parent: Option<ParentRef>,
    working_dir: PathBuf,
    state: SessionState,
}

impl Session {
    /// 新根会话（无父节点，初始静止）。
    pub(crate) fn root(working_dir: PathBuf) -> Self {
        Self {
            id: SessionId::new(),
            parent: None,
            working_dir,
            state: SessionState::Idle,
        }
    }

    /// 分叉出的子会话：继承工作目录，初始静止，消息按写时复制语义
    /// 由存储层在首次写入时从父会话分叉点复制。
    pub(crate) fn forked_from(parent: &Session, forked_at: MessageId) -> Self {
        Self {
            id: SessionId::new(),
            parent: Some(ParentRef {
                session: parent.id,
                forked_at,
            }),
            working_dir: parent.working_dir.clone(),
            state: SessionState::Idle,
        }
    }

    /// 会话 id。
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// 父节点引用；根会话为 `None`。
    #[must_use]
    pub const fn parent(&self) -> Option<ParentRef> {
        self.parent
    }

    /// 会话绑定的启动工作目录；工具操作不得越出该目录。
    #[must_use]
    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    /// 当前执行状态。
    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 合法状态转换全部成功() {
        assert_eq!(
            SessionState::Pending.dispatch().unwrap(),
            SessionState::Running
        );
        assert_eq!(
            SessionState::Running.suspend().unwrap(),
            SessionState::Pending
        );
        assert_eq!(SessionState::Running.quiesce().unwrap(), SessionState::Idle);
    }

    #[test]
    fn 非法状态转换返回错误() {
        // 静止会话没有待处理工作，不可被直接分发
        assert!(SessionState::Idle.dispatch().is_err());
        assert!(SessionState::Running.dispatch().is_err());
        assert!(SessionState::Idle.suspend().is_err());
        assert!(SessionState::Pending.quiesce().is_err());
        assert!(SessionState::Idle.quiesce().is_err());
    }

    #[test]
    fn 根会话无父节点且初始静止() {
        let session = Session::root(PathBuf::from("/tmp/ws"));
        assert_eq!(session.state(), SessionState::Idle);
        assert_eq!(session.parent(), None);
        assert_eq!(session.working_dir(), Path::new("/tmp/ws"));
    }

    #[test]
    fn 分叉会话继承工作目录并记录分叉点() {
        let parent = Session::root(PathBuf::from("/tmp/ws"));
        let forked_at = MessageId::new();
        let child = Session::forked_from(&parent, forked_at);
        assert_eq!(child.parent().unwrap().session, parent.id());
        assert_eq!(child.parent().unwrap().forked_at, forked_at);
        assert_eq!(child.working_dir(), parent.working_dir());
        assert_eq!(child.state(), SessionState::Idle);
        assert_ne!(child.id(), parent.id());
    }

    #[test]
    fn 会话json_往返保持值() {
        let session = Session::root(PathBuf::from("/tmp/ws"));
        let json = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), session);
    }
}
