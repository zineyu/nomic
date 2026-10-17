//! 历史回放：落库步骤 → LLM 请求消息序列的显式构造。
//!
//! 回放与恢复走同一条路径：消息步骤原样进入序列；工具调用步骤
//! 配对构造工具结果消息（`ToolCallStarted` 必须与随后的
//! `ToolCallFinished` / `ToolCallInterrupted` 配对）；被标记
//! interrupted 的调用以其已提交的部分输出回灌模型。

use nomic_core::{Message, MessageId, Step, ToolCall, ToolCallId, ToolResult};

use crate::agent::AgentError;

/// 回放核心结果：消息序列与末尾回合的未闭合状态。
struct Replayed {
    messages: Vec<Message>,
    /// 末尾回合已开始、未闭合的调用。
    in_flight: Option<ToolCall>,
    /// 末尾回合声明但从未开始的调用。
    unstarted: Vec<ToolCall>,
}

/// 重驱面待处理的工具调用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingCall {
    /// 已开始、未闭合：replay-safe 续跑一次；非 replay-safe 标记
    /// interrupted，副作用绝不重复执行。
    InFlight(ToolCall),
    /// 已声明、从未开始：`ToolCallStarted` 先于派发落库，未开始即
    /// 副作用未发生，可直接续跑（与 replay-safe 声明无关）。
    Unstarted(ToolCall),
}

/// 落库步骤回放为 LLM 请求消息序列（不含系统提示）。
///
/// 历史必须完整：工具调用开始与结束配对，助手消息声明的调用都已
/// 有结果。违反时返回 [`AgentError::BrokenHistory`]。
pub fn replay_messages(steps: &[Step]) -> Result<Vec<Message>, AgentError> {
    let replayed = replay(steps)?;
    if let Some(open) = replayed.in_flight {
        return Err(AgentError::BrokenHistory(format!(
            "tool call {} started without completion",
            open.id
        )));
    }
    if let Some(call) = replayed.unstarted.first() {
        return Err(AgentError::BrokenHistory(format!(
            "tool call {} declared without start",
            call.id
        )));
    }
    Ok(replayed.messages)
}

/// 展示回放：末尾回合未闭合的工具调用暂不产出结果消息。
///
/// 崩溃残留的会话处于「已声明未开始」或「已开始未闭合」状态，
/// 由重驱路径闭合后再推送结果；展示路径不因该状态失败。非末尾的
/// 配对违约仍报破损。
pub fn partial_messages(steps: &[Step]) -> Result<Vec<Message>, AgentError> {
    Ok(replay(steps)?.messages)
}

/// 末尾回合待处理的工具调用；无则为 `None`。
///
/// 恢复路径据此判断续跑还是标记 interrupted。已开始的调用优先
/// （必须先闭合才能续跑同回合的后续调用）；其次按声明顺序返回
/// 未开始的调用。非末尾的配对违约仍报破损。
pub fn pending_call(steps: &[Step]) -> Result<Option<PendingCall>, AgentError> {
    let replayed = replay(steps)?;
    if let Some(call) = replayed.in_flight {
        return Ok(Some(PendingCall::InFlight(call)));
    }
    Ok(replayed
        .unstarted
        .into_iter()
        .next()
        .map(PendingCall::Unstarted))
}

/// 回放核心：消息步骤原样进入序列；工具调用步骤配对构造结果消息
/// （`ToolCallStarted` 必须与随后的 `ToolCallFinished` /
/// `ToolCallInterrupted` 配对）；被标记 interrupted 的调用以其
/// 已提交的部分输出回灌模型。
///
/// 助手消息声明了本回合的调用。声明与开始是两次独立 checkpoint：
/// 崩溃可落在两者之间，因此末尾回合声明的调用可能尚未开始。
fn replay(steps: &[Step]) -> Result<Replayed, AgentError> {
    let mut messages = Vec::with_capacity(steps.len());
    let mut in_flight: Option<ToolCall> = None;
    // 当前助手回合声明的调用与其中已落库开始的调用
    let mut declared: Vec<ToolCall> = Vec::new();
    let mut started: Vec<ToolCallId> = Vec::new();
    for step in steps {
        match step {
            Step::Message(message @ Message::Assistant { tool_calls, .. }) => {
                // 上一回合必须已全部闭合才能出现下一条助手消息
                ensure_turn_closed(in_flight.as_ref(), &declared, &started)?;
                declared.clone_from(tool_calls);
                started.clear();
                messages.push(message.clone());
            }
            Step::Message(message) => messages.push(message.clone()),
            Step::ToolCallStarted { call } => {
                if let Some(open) = in_flight.replace(call.clone()) {
                    return Err(AgentError::BrokenHistory(format!(
                        "tool call {} started while {} is still in flight",
                        call.id, open.id
                    )));
                }
                started.push(call.id);
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
                messages.push(interrupted_message(open.id, partial_output.as_deref()));
            }
        }
    }
    let unstarted = declared
        .into_iter()
        .filter(|call| !started.contains(&call.id))
        .collect();
    Ok(Replayed {
        messages,
        in_flight,
        unstarted,
    })
}

/// 上一条助手消息声明的调用必须已全部开始并闭合。
fn ensure_turn_closed(
    in_flight: Option<&ToolCall>,
    declared: &[ToolCall],
    started: &[ToolCallId],
) -> Result<(), AgentError> {
    if let Some(open) = in_flight {
        return Err(AgentError::BrokenHistory(format!(
            "tool call {} started without completion before next assistant message",
            open.id
        )));
    }
    if let Some(call) = declared.iter().find(|call| !started.contains(&call.id)) {
        return Err(AgentError::BrokenHistory(format!(
            "tool call {} declared without start before next assistant message",
            call.id
        )));
    }
    Ok(())
}

/// 取走与 `call_id` 配对的进行中调用；缺失或不匹配即历史破损。
fn take_in_flight(
    in_flight: &mut Option<ToolCall>,
    call_id: ToolCallId,
) -> Result<ToolCall, AgentError> {
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

/// interrupted 调用的回灌消息：已提交的部分输出随中断说明回灌，
/// 无已提交输出时回灌中断说明。归为工具错误层（调用未完成），
/// 绝不归类为成功输出。
pub fn interrupted_message(call_id: ToolCallId, partial_output: Option<&str>) -> Message {
    let result = match partial_output {
        Some(output) => ToolResult::error(format!(
            "tool call interrupted before completion; partial output committed:\n{output}"
        )),
        None => ToolResult::error("tool call interrupted before completion; no output committed"),
    };
    Message::Tool {
        id: MessageId::new(),
        call_id,
        result,
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
    fn 末尾在途调用_展示回放产出已有消息_恢复面定位已开始调用() {
        let call = sample_call();
        let steps = vec![
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "任务".into(),
            }),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: String::new(),
                tool_calls: vec![call.clone()],
            }),
            started(call.clone()),
        ];
        // 展示回放：未闭合调用不产出结果消息
        let messages = partial_messages(&steps).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role(), nomic_core::Role::User);
        // 在途调用可定位，供恢复路径续跑或标记 interrupted
        assert_eq!(
            pending_call(&steps).unwrap(),
            Some(PendingCall::InFlight(call))
        );
        // 完整回放（LLM 请求）要求已闭合
        assert!(matches!(
            replay_messages(&steps).unwrap_err(),
            AgentError::BrokenHistory(_)
        ));
    }

    #[test]
    fn 声明未开始的调用_展示回放容忍_完整回放报破损() {
        let call = sample_call();
        let assistant = Step::Message(Message::Assistant {
            id: MessageId::new(),
            text: String::new(),
            tool_calls: vec![call.clone()],
        });
        let steps = vec![
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "任务".into(),
            }),
            assistant.clone(),
        ];
        // 崩溃可能落在「声明已落库」与「开始已落库」之间
        assert_eq!(partial_messages(&steps).unwrap().len(), 2);
        assert_eq!(
            pending_call(&steps).unwrap(),
            Some(PendingCall::Unstarted(call.clone()))
        );
        assert!(matches!(
            replay_messages(&steps).unwrap_err(),
            AgentError::BrokenHistory(_)
        ));
        // 全部闭合后能完整回放，恢复面无待处理调用
        let closed = vec![
            steps[0].clone(),
            assistant,
            started(call.clone()),
            Step::ToolCallFinished {
                call_id: call.id,
                result: ToolResult::ok("输出"),
            },
        ];
        assert!(pending_call(&closed).unwrap().is_none());
        assert!(replay_messages(&closed).is_ok());
    }

    #[test]
    fn 同回合未开始的调用按声明顺序续跑() {
        let first = sample_call();
        let second = sample_call();
        let steps = vec![
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: String::new(),
                tool_calls: vec![first.clone(), second.clone()],
            }),
            started(first.clone()),
            Step::ToolCallFinished {
                call_id: first.id,
                result: ToolResult::ok("输出一"),
            },
        ];
        assert_eq!(
            pending_call(&steps).unwrap(),
            Some(PendingCall::Unstarted(second))
        );
    }

    #[test]
    fn 非末尾回合声明未开始_历史破损() {
        let call = sample_call();
        let steps = vec![
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: String::new(),
                tool_calls: vec![call],
            }),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: "继续".into(),
                tool_calls: Vec::new(),
            }),
        ];
        assert!(matches!(
            partial_messages(&steps).unwrap_err(),
            AgentError::BrokenHistory(_)
        ));
        assert!(matches!(
            replay_messages(&steps).unwrap_err(),
            AgentError::BrokenHistory(_)
        ));
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
