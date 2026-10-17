//! 受理请求（submission）与受理结果。
//!
//! 客户端每次提交携带自己生成的 [`RequestId`]；服务端在提交层
//! 按 requestId 去重——重复提交视为同一受理，绝不重复进入会话。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::id::{RequestId, SessionId};
use crate::message::Message;

/// 提交的目标会话。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTarget {
    /// 新会话：由服务端创建后受理。
    New,
    /// 已有会话。
    Existing(SessionId),
}

/// 客户端的一次提交：用户消息进入会话的受理单元。
///
/// `request_id` 是去重键：同一 `request_id` 的重复提交
/// （网络重发、客户端重试）必须得到同一受理结果，消息不得
/// 重复进入会话。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    /// 客户端生成的受理请求 id。
    pub request_id: RequestId,
    /// 目标会话。
    pub target: SessionTarget,
    /// 提交来源的工作目录；目标为新建会话时服务端据以创建会话。
    /// 它是持久化的工作区标识（参照 Pi durable 的会话头 cwd），
    /// 坐标系由执行环境解释（本地路径或沙箱内路径）。
    pub working_dir: PathBuf,
    /// 用户消息文本。
    pub content: String,
}

/// 受理结果。
///
/// `Duplicate` 不是失败：它与 `Accepted` 同样给出会话 id，
/// 客户端据此得知「已受理过，无需重试」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acceptance {
    /// 首次受理，消息已进入目标会话。
    Accepted {
        /// 目标会话 id；`SessionTarget::New` 时为新建会话 id。
        session_id: SessionId,
    },
    /// 同一 requestId 已受理过，返回当时的受理结果。
    Duplicate {
        /// 原受理的目标会话 id。
        session_id: SessionId,
    },
}

impl Acceptance {
    /// 受理（或已受理过）的会话 id。
    #[must_use]
    pub const fn session_id(self) -> SessionId {
        match self {
            Self::Accepted { session_id } | Self::Duplicate { session_id } => session_id,
        }
    }
}

/// 受理写入结果：受理判定与（可立即排入时）进入会话的用户消息。
///
/// 服务端据 `message` 把用户输入广播给该会话订阅者。会话空闲时
/// 消息立即排入步骤日志，`message` 为其真值；会话运行中时消息
/// 持久化为待处理提交，由 worker 在回合边界排入并广播，此时
/// `message` 为 `None`；重复受理也为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionRecord {
    /// 受理判定（首次受理或重复受理）。
    pub acceptance: Acceptance,
    /// 可立即排入步骤日志的用户消息；未排入（运行中推迟或重复受理）为 `None`。
    pub message: Option<Message>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_两分支都携带会话_id() {
        let session_id = SessionId::new();
        let accepted = Acceptance::Accepted { session_id };
        let duplicate = Acceptance::Duplicate { session_id };
        assert!(matches!(accepted, Acceptance::Accepted { .. }));
        assert!(matches!(duplicate, Acceptance::Duplicate { .. }));
    }

    #[test]
    fn submission_json_往返保持值() {
        for submission in [
            Submission {
                request_id: RequestId::new(),
                target: SessionTarget::New,
                working_dir: PathBuf::from("/tmp/ws"),
                content: "新任务".into(),
            },
            Submission {
                request_id: RequestId::new(),
                target: SessionTarget::Existing(SessionId::new()),
                working_dir: PathBuf::from("/tmp/ws"),
                content: "继续".into(),
            },
        ] {
            let json = serde_json::to_string(&submission).unwrap();
            assert_eq!(
                serde_json::from_str::<Submission>(&json).unwrap(),
                submission
            );
        }
    }
}
