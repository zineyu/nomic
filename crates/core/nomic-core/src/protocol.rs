//! IPC 协议消息类型。
//!
//! Unix socket 上的 JSON Lines 双向消息：连接首帧交换协议版本，
//! 之后客户端发送请求、服务端回执并推送事件。协议为未来的
//! 非 TUI 客户端预留。

use serde::{Deserialize, Serialize};

use crate::event::Event;
use crate::id::{MessageId, RequestId, SessionId};
use crate::submission::{Acceptance, Submission};
use crate::tree::SessionTreeSnapshot;

/// 当前协议版本；连接首帧交换，不兼容版本拒绝连接。
pub const PROTOCOL_VERSION: u32 = 2;

/// 客户端 → 服务端消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    /// 连接首帧：声明协议版本。
    Hello {
        /// 客户端支持的协议版本。
        protocol_version: u32,
    },
    /// 提交用户消息（受理层按 requestId 去重）。
    Submit {
        /// 受理请求。
        submission: Submission,
    },
    /// 取消会话当前进行中的模型请求或工具执行（US5）。
    Cancel {
        /// 会话 id。
        session_id: SessionId,
    },
    /// 订阅会话事件流。
    Subscribe {
        /// 会话 id。
        session_id: SessionId,
    },
    /// 请求会话树快照（用于列表/树形展示）。
    ListSessions,
    /// 恢复会话：请求回放历史步骤与后续事件。
    Resume {
        /// 会话 id。
        session_id: SessionId,
    },
    /// 在任意消息处分叉会话。
    Fork {
        /// 来源会话 id。
        session_id: SessionId,
        /// 分叉点消息 id。
        at: MessageId,
    },
    /// 删除历史会话。
    Delete {
        /// 会话 id。
        session_id: SessionId,
    },
}

/// 服务端 → 客户端消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMessage {
    /// 连接首帧应答：服务端协议版本。
    HelloAck {
        /// 服务端协议版本。
        protocol_version: u32,
    },
    /// 提交回执：受理结果（含去重后的重复受理）。
    SubmissionReceipt {
        /// 对应提交的 requestId。
        request_id: RequestId,
        /// 受理结果。
        acceptance: Acceptance,
    },
    /// 订阅会话的事件流。
    Event(Event),
    /// 会话树快照响应：对列表/恢复/分叉/删除请求的应答。
    ///
    /// 与广播事件 [`Event::SessionTreeChanged`] 区分：本帧只回发起
    /// 连接的请求，广播事件推送给全部连接。
    SessionTree {
        /// 当前会话树快照（只含元数据，不含消息体）。
        tree: SessionTreeSnapshot,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::submission::SessionTarget;
    use crate::tree::SessionTreeSnapshot;

    #[test]
    fn 协议版本为_2() {
        assert_eq!(PROTOCOL_VERSION, 2);
    }

    #[test]
    fn 客户端消息每个分支_json_lines_往返保持值() {
        let messages = [
            ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
            ClientMessage::Submit {
                submission: Submission {
                    request_id: RequestId::new(),
                    target: SessionTarget::New,
                    working_dir: std::path::PathBuf::from("/ws"),
                    content: "任务".into(),
                },
            },
            ClientMessage::Cancel {
                session_id: SessionId::new(),
            },
            ClientMessage::Subscribe {
                session_id: SessionId::new(),
            },
            ClientMessage::ListSessions,
            ClientMessage::Resume {
                session_id: SessionId::new(),
            },
            ClientMessage::Fork {
                session_id: SessionId::new(),
                at: MessageId::new(),
            },
            ClientMessage::Delete {
                session_id: SessionId::new(),
            },
        ];
        for message in messages {
            // 协议载体是 JSON Lines：每帧恰好一行
            let line = serde_json::to_string(&message).unwrap();
            assert!(!line.contains('\n'));
            assert_eq!(
                serde_json::from_str::<ClientMessage>(&line).unwrap(),
                message
            );
        }
    }

    #[test]
    fn 服务端消息每个分支_json_lines_往返保持值() {
        let messages = [
            ServerMessage::HelloAck {
                protocol_version: PROTOCOL_VERSION,
            },
            ServerMessage::SubmissionReceipt {
                request_id: RequestId::new(),
                acceptance: Acceptance::Accepted {
                    session_id: SessionId::new(),
                },
            },
            ServerMessage::Event(Event::SessionTreeChanged {
                tree: SessionTreeSnapshot::default(),
            }),
            ServerMessage::SessionTree {
                tree: SessionTreeSnapshot::default(),
            },
        ];
        for message in messages {
            let line = serde_json::to_string(&message).unwrap();
            assert!(!line.contains('\n'));
            assert_eq!(
                serde_json::from_str::<ServerMessage>(&line).unwrap(),
                message
            );
        }
    }

    #[test]
    fn 协议帧为人类可读的_snake_case_标签() {
        let hello = serde_json::to_value(ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        })
        .unwrap();
        assert!(hello.get("hello").is_some());
    }
}
