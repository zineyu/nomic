//! 受理请求（submission）与受理结果。
//!
//! 客户端每次提交携带自己生成的 [`RequestId`]；服务端在提交层
//! 按 requestId 去重——重复提交视为同一受理，绝不重复进入会话。

use serde::{Deserialize, Serialize};

use crate::id::{RequestId, SessionId};

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
                content: "新任务".into(),
            },
            Submission {
                request_id: RequestId::new(),
                target: SessionTarget::Existing(SessionId::new()),
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
