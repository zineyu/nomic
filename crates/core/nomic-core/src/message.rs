//! 消息领域类型。
//!
//! 消息用枚举变体表达：角色与内容形态的配对由类型结构保证，
//! 反序列化无法构造「Tool 角色携带文本」之类的非法组合。

use serde::{Deserialize, Serialize};

use crate::id::{MessageId, ToolCallId};
use crate::tool::{ToolCall, ToolResult};

/// 消息角色，供 LLM 适配层映射到 provider 协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// 系统提示，由内置默认提示与工作目录 AGENTS.md / CLAUDE.md 组装。
    System,
    /// 用户输入。
    User,
    /// 模型输出，可携带工具调用请求。
    Assistant,
    /// 工具结果回灌。
    Tool,
}

/// 会话中的一条消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// 系统提示消息。
    System {
        /// 消息 id。
        id: MessageId,
        /// 提示文本。
        text: String,
    },
    /// 用户消息。
    User {
        /// 消息 id。
        id: MessageId,
        /// 用户输入文本。
        text: String,
    },
    /// 模型输出消息；纯工具调用回合的 `text` 可为空字符串。
    Assistant {
        /// 消息 id。
        id: MessageId,
        /// 已完成的输出文本（流式增量汇总的最终结果）。
        text: String,
        /// 本回合请求的工具调用，可为空。
        tool_calls: Vec<ToolCall>,
    },
    /// 工具结果消息，回灌给模型。
    Tool {
        /// 消息 id。
        id: MessageId,
        /// 对应工具调用的 id。
        call_id: ToolCallId,
        /// 工具执行结果。
        result: ToolResult,
    },
}

impl Message {
    /// 消息 id。
    #[must_use]
    pub const fn id(&self) -> MessageId {
        match self {
            Self::System { id, .. }
            | Self::User { id, .. }
            | Self::Assistant { id, .. }
            | Self::Tool { id, .. } => *id,
        }
    }

    /// 消息角色。
    #[must_use]
    pub const fn role(&self) -> Role {
        match self {
            Self::System { .. } => Role::System,
            Self::User { .. } => Role::User,
            Self::Assistant { .. } => Role::Assistant,
            Self::Tool { .. } => Role::Tool,
        }
    }

    /// 文本内容；`Tool` 消息返回输出或拒绝原因。
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::System { text, .. } | Self::User { text, .. } | Self::Assistant { text, .. } => {
                text
            }
            Self::Tool { result, .. } => match result {
                ToolResult::Ok { output } => output,
                ToolResult::Rejected { reason } => reason,
            },
        }
    }

    /// 助手消息的工具调用；其余角色恒为空切片。
    #[must_use]
    pub fn tool_calls(&self) -> &[ToolCall] {
        match self {
            Self::Assistant { tool_calls, .. } => tool_calls,
            Self::System { .. } | Self::User { .. } | Self::Tool { .. } => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 每种角色的_role_与_id_accessor_正确() {
        let id = MessageId::new();
        let msg = Message::User {
            id,
            text: "hello".into(),
        };
        assert_eq!(msg.role(), Role::User);
        assert_eq!(msg.id(), id);

        let tool_msg = Message::Tool {
            id: MessageId::new(),
            call_id: ToolCallId::new(),
            result: ToolResult::ok("42"),
        };
        assert_eq!(tool_msg.role(), Role::Tool);
        assert!(tool_msg.tool_calls().is_empty());
        assert_eq!(tool_msg.text(), "42");
    }

    #[test]
    fn 助手消息携带工具调用() {
        let call = ToolCall {
            id: ToolCallId::new(),
            tool: crate::tool::ToolName::new("list_dir"),
            arguments: serde_json::json!({}),
        };
        let msg = Message::Assistant {
            id: MessageId::new(),
            text: String::new(),
            tool_calls: vec![call.clone()],
        };
        assert_eq!(msg.tool_calls(), &[call]);
    }

    #[test]
    fn 消息_json_往返保持值() {
        for msg in [
            Message::System {
                id: MessageId::new(),
                text: "system".into(),
            },
            Message::User {
                id: MessageId::new(),
                text: "user".into(),
            },
            Message::Tool {
                id: MessageId::new(),
                call_id: ToolCallId::new(),
                result: ToolResult::rejected("denied"),
            },
        ] {
            let json = serde_json::to_string(&msg).unwrap();
            assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
        }
    }

    #[test]
    fn 变体标签为_snake_case_可读形式() {
        let msg = Message::User {
            id: MessageId::new(),
            text: "hi".into(),
        };
        let value: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert!(value.get("user").is_some());
    }
}
