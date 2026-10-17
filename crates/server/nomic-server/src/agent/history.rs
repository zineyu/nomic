//! 历史回放：落库步骤 → LLM 请求消息序列的显式构造。
//!
//! 回放与恢复走同一条路径：消息步骤原样进入序列；工具调用步骤
//! 配对构造工具结果消息（`ToolCallStarted` 必须与随后的
//! `ToolCallFinished` / `ToolCallInterrupted` 配对）；被标记
//! interrupted 的调用以其已提交的部分输出回灌模型。

use nomic_core::{Message, MessageId, Step, ToolResult};

use crate::agent::AgentError;

/// 落库步骤回放为 LLM 请求消息序列（不含系统提示）。
///
/// 步骤序列违反配对不变式（开始缺少结束、结束缺少开始、
/// 调用 id 不匹配）时返回 [`AgentError::BrokenHistory`]。
pub(super) fn replay_messages(steps: &[Step]) -> Result<Vec<Message>, AgentError> {
    let mut messages = Vec::with_capacity(steps.len());
    let mut in_flight: Option<&nomic_core::ToolCall> = None;
    for step in steps {
        match step {
            Step::Message(message) => messages.push(message.clone()),
            Step::ToolCallStarted { call } => {
                if let Some(open) = in_flight.replace(call) {
                    return Err(AgentError::BrokenHistory(format!(
                        "tool call {} started while {} is still in flight",
                        call.id, open.id
                    )));
                }
            }
            Step::ToolCallFinished { call_id, result } => {
                let open = take_in_flight(&mut in_flight, *call_id)?;
                messages.push(Message::Tool {
                    id: MessageId::new(),
                    call_id: open.id,
                    result: result.clone(),
                });
            }
            Step::ToolCallInterrupted {
                call_id,
                partial_output,
            } => {
                let open = take_in_flight(&mut in_flight, *call_id)?;
                messages.push(Message::Tool {
                    id: MessageId::new(),
                    call_id: open.id,
                    result: interrupted_result(partial_output.as_ref()),
                });
            }
        }
    }
    if let Some(open) = in_flight {
        return Err(AgentError::BrokenHistory(format!(
            "tool call {} started without completion",
            open.id
        )));
    }
    Ok(messages)
}

/// 取走与 `call_id` 配对的进行中调用；缺失或不匹配即历史破损。
fn take_in_flight<'a>(
    in_flight: &mut Option<&'a nomic_core::ToolCall>,
    call_id: nomic_core::ToolCallId,
) -> Result<&'a nomic_core::ToolCall, AgentError> {
    match in_flight.take() {
        Some(open) if open.id == call_id => Ok(open),
        Some(open) => Err(AgentError::BrokenHistory(format!(
            "tool call completion {call_id} does not match started call {}",
            open.id
        ))),
        None => Err(AgentError::BrokenHistory(format!(
            "tool call {call_id} completed without start"
        ))),
    }
}

/// interrupted 调用的回灌结果：已提交的部分输出随中断说明回灌，
/// 无已提交输出时回灌中断说明。归为工具错误层（调用未完成），
/// 绝不归类为成功输出。
fn interrupted_result(partial_output: Option<&String>) -> ToolResult {
    match partial_output {
        Some(output) => ToolResult::error(format!(
            "tool call interrupted before completion; partial output committed:\n{output}"
        )),
        None => ToolResult::error("tool call interrupted before completion; no output committed"),
    }
}

#[cfg(test)]
mod tests {
    use nomic_core::{ToolCall, ToolCallId, ToolName};

    use super::*;

    fn started(call: ToolCall) -> Step {
        Step::ToolCallStarted { call }
    }

    fn sample_call() -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            tool: ToolName::new("shell"),
            arguments: serde_json::json!({"command": "cargo test"}),
        }
    }

    #[test]
    fn 消息步骤原样回放() {
        let steps = vec![
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "任务".into(),
            }),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: "回复".into(),
                tool_calls: Vec::new(),
            }),
        ];
        let messages = replay_messages(&steps).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role(), nomic_core::Role::User);
    }

    #[test]
    fn 工具步骤配对构造工具结果消息() {
        let call = sample_call();
        let steps = vec![
            started(call.clone()),
            Step::ToolCallFinished {
                call_id: call.id,
                result: ToolResult::ok("输出"),
            },
        ];
        let messages = replay_messages(&steps).unwrap();
        match &messages[..] {
            [
                Message::Tool {
                    call_id, result, ..
                },
            ] => {
                assert_eq!(*call_id, call.id);
                assert!(matches!(result, ToolResult::Ok { output } if output == "输出"));
            }
            other => panic!("应为一条工具结果消息: {other:?}"),
        }
    }

    #[test]
    fn interrupted_调用以部分输出回灌为工具错误层() {
        let call = sample_call();
        let steps = vec![
            started(call.clone()),
            Step::ToolCallInterrupted {
                call_id: call.id,
                partial_output: Some("部分日志".into()),
            },
        ];
        let messages = replay_messages(&steps).unwrap();
        match &messages[..] {
            [Message::Tool { result, .. }] => match result {
                ToolResult::Error { message } => {
                    assert!(message.contains("interrupted"));
                    assert!(message.contains("部分日志"));
                }
                other => panic!("interrupted 结果必须为错误层: {other:?}"),
            },
            other => panic!("应为一条工具结果消息: {other:?}"),
        }
    }

    #[test]
    fn 配对不变式破坏_显式失败() {
        let call = sample_call();
        // 开始缺少结束
        let err = replay_messages(&[started(call.clone())]).unwrap_err();
        assert!(matches!(err, AgentError::BrokenHistory(_)), "实际: {err}");
        // 结束缺少开始
        let err = replay_messages(&[Step::ToolCallFinished {
            call_id: call.id,
            result: ToolResult::ok("x"),
        }])
        .unwrap_err();
        assert!(matches!(err, AgentError::BrokenHistory(_)), "实际: {err}");
        // id 不匹配
        let err = replay_messages(&[
            started(call.clone()),
            Step::ToolCallFinished {
                call_id: ToolCallId::new(),
                result: ToolResult::ok("x"),
            },
        ])
        .unwrap_err();
        assert!(matches!(err, AgentError::BrokenHistory(_)), "实际: {err}");
        // 连续开始
        let err = replay_messages(&[started(call.clone()), started(call)]).unwrap_err();
        assert!(matches!(err, AgentError::BrokenHistory(_)), "实际: {err}");
    }
}
