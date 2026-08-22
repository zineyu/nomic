//! 运行状态提示阶段推导测试（输入框上方单行提示，`App::run_phase`）。

use super::*;

/// 阶段随聊天区尾部状态切换：等输出 → thinking → 正文 → 工具 → 等下一轮；
/// 空闲（非运行中）无阶段。
#[test]
fn run_phase_follows_chat_tail() {
    let mut app = app();
    // 空闲：无阶段
    assert_eq!(app.run_phase(), None);

    // 已启动、尚无输出：等输出
    app.handle_event(&AgentEvent::AgentStart);
    assert_eq!(app.run_phase(), Some(RunPhase::Waiting));

    // assistant 消息开始但还没有内容块：仍等输出
    app.handle_event(&AgentEvent::MessageStart(assistant_message(
        Vec::new(),
        StopReason::Stop,
        None,
    )));
    assert_eq!(app.run_phase(), Some(RunPhase::Waiting));

    // thinking 流式输出中
    app.handle_event(&AgentEvent::MessageUpdate(AssistantEvent::ThinkingStart {
        index: 0,
    }));
    assert_eq!(app.run_phase(), Some(RunPhase::Thinking));

    // 正文流式输出中
    app.handle_event(&AgentEvent::MessageUpdate(AssistantEvent::TextStart {
        index: 1,
    }));
    assert_eq!(app.run_phase(), Some(RunPhase::Writing));

    // 工具执行中（优先于 assistant 流式状态）
    app.handle_event(&AgentEvent::ToolExecutionStart {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        args: serde_json::json!({}),
    });
    assert_eq!(app.run_phase(), Some(RunPhase::ToolCalling));
    assert_eq!(
        app.running_tool().map(|tool| tool.name.as_str()),
        Some("bash")
    );

    // 工具结束、等下一轮回复：回到等输出
    app.handle_event(&AgentEvent::ToolExecutionEnd {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        result: ToolResult::text("ok"),
        is_error: false,
    });
    assert_eq!(app.run_phase(), Some(RunPhase::Waiting));

    // 本轮结束（运行标志清除）：无阶段
    app.finish_run(None);
    assert_eq!(app.run_phase(), None);
}

/// 历史遗留的失败工具不影响阶段推导（只看仍在运行的工具）。
#[test]
fn run_phase_ignores_finished_tools() {
    let mut app = app();
    app.handle_event(&AgentEvent::ToolExecutionStart {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        args: serde_json::json!({}),
    });
    app.handle_event(&AgentEvent::ToolExecutionEnd {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        result: ToolResult::text("boom"),
        is_error: true,
    });
    app.handle_event(&AgentEvent::AgentStart);
    assert_eq!(app.run_phase(), Some(RunPhase::Waiting));
}
