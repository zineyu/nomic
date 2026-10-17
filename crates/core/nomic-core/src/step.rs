//! 会话步骤：checkpoint 的持久化单元。
//!
//! agent loop 每步完成即落库一个 [`Step`]；崩溃恢复与用户 resume
//! 走同一条回放路径；事件与 checkpoint 步骤一一对应（流式增量除外）。

use serde::{Deserialize, Serialize};

use crate::id::ToolCallId;
use crate::message::Message;
use crate::tool::{ToolCall, ToolResult};

/// 会话中一个已确认的步骤。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// 一条消息（用户提交、助手回复、系统提示、工具结果回灌）。
    Message(Message),
    /// 工具调用开始执行。
    ToolCallStarted {
        /// 调用内容。
        call: ToolCall,
    },
    /// 工具调用执行完成，结果已定（含被拒绝）。
    ToolCallFinished {
        /// 调用 id。
        call_id: ToolCallId,
        /// 执行结果。
        result: ToolResult,
    },
    /// 未完成的工具调用被标记为 interrupted：已派发但中断前未产出
    /// 最终结果（如 server 崩溃）。恢复时其已提交的部分输出回灌模型，
    /// 副作用绝不重复执行。
    ToolCallInterrupted {
        /// 调用 id。
        call_id: ToolCallId,
        /// 中断前已提交的部分输出；无已提交输出时为 `None`。
        partial_output: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{MessageId, ToolCallId};
    use crate::tool::{ToolCall, ToolName, ToolResult};

    #[test]
    fn step_各分支_json_往返保持值() {
        let call = ToolCall {
            id: ToolCallId::new(),
            tool: ToolName::new("read_file"),
            arguments: serde_json::json!({"path": "a.rs"}),
        };
        for step in [
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "hi".into(),
            }),
            Step::ToolCallStarted { call: call.clone() },
            Step::ToolCallFinished {
                call_id: call.id,
                result: ToolResult::rejected("denied"),
            },
            Step::ToolCallInterrupted {
                call_id: call.id,
                partial_output: Some("partial stdout".into()),
            },
            Step::ToolCallInterrupted {
                call_id: ToolCallId::new(),
                partial_output: None,
            },
        ] {
            let json = serde_json::to_string(&step).unwrap();
            assert_eq!(serde_json::from_str::<Step>(&json).unwrap(), step);
        }
    }
}
