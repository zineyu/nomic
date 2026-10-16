//! 服务端事件：server → 客户端的推送消息。
//!
//! 订阅会话的客户端收到事件流；事件与落库步骤一一对应，
//! 断线重连后可经回放补齐。

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SessionId, ToolCallId};
use crate::message::Message;
use crate::tool::{ToolCall, ToolResult};
use crate::tree::SessionTreeSnapshot;

/// 事件中的错误类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventErrorKind {
    /// 网络层瞬时错误之外的 LLM 传输故障（瞬时错误的重试在 server 内完成）。
    Llm,
    /// 执行环境自身故障。
    Environment,
    /// 会话达到上下文窗口上限，agent 停止（US7：用户可分叉续跑）。
    ContextLimit,
    /// 会话被用户取消（US5）。
    Cancelled,
    /// 其他内部错误。
    Internal,
}

/// 服务端推送给客户端的事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// 流式输出增量。
    TokenDelta {
        /// 会话 id。
        session_id: SessionId,
        /// 正在生成的助手消息 id。
        message_id: MessageId,
        /// 文本增量。
        delta: String,
    },
    /// 一条消息完成（含工具结果回灌完成）。
    MessageCompleted {
        /// 会话 id。
        session_id: SessionId,
        /// 完成的消息。
        message: Message,
    },
    /// 工具调用开始执行。
    ToolCallStarted {
        /// 会话 id。
        session_id: SessionId,
        /// 调用内容。
        call: ToolCall,
    },
    /// 工具调用执行完成。
    ToolCallFinished {
        /// 会话 id。
        session_id: SessionId,
        /// 调用 id。
        call_id: ToolCallId,
        /// 执行结果。
        result: ToolResult,
    },
    /// 执行出错，会话停在可继续状态。
    Error {
        /// 会话 id。
        session_id: SessionId,
        /// 错误类别。
        kind: EventErrorKind,
        /// 面向用户的错误说明。
        message: String,
    },
    /// 会话树发生变化（新建、分叉、删除）。
    SessionTreeChanged {
        /// 变化后的会话树快照（只含元数据，不含消息体）。
        tree: SessionTreeSnapshot,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{MessageId, SessionId, ToolCallId};
    use crate::tool::{ToolCall, ToolName, ToolResult};

    fn sample_call() -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            tool: ToolName::new("shell"),
            arguments: serde_json::json!({"command": "cargo test"}),
        }
    }

    #[test]
    fn 每个事件分支_json_往返保持值() {
        let session_id = SessionId::new();
        let message_id = MessageId::new();
        let call = sample_call();
        let events = [
            Event::TokenDelta {
                session_id,
                message_id,
                delta: "你好".into(),
            },
            Event::MessageCompleted {
                session_id,
                message: Message::Assistant {
                    id: message_id,
                    text: "hello".into(),
                    tool_calls: vec![],
                },
            },
            Event::ToolCallStarted {
                session_id,
                call: call.clone(),
            },
            Event::ToolCallFinished {
                session_id,
                call_id: call.id,
                result: ToolResult::ok("done"),
            },
            Event::Error {
                session_id,
                kind: EventErrorKind::ContextLimit,
                message: "接近上下文上限".into(),
            },
            Event::SessionTreeChanged {
                tree: SessionTreeSnapshot::default(),
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), event);
        }
    }
}
