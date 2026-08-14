//! agent loop 集成测试：用脚本化 mock provider 验证 loop 行为。

mod injection;
mod support;

use std::sync::Arc;

use async_trait::async_trait;
use nomic_ai::{
    AssistantContent, AssistantEvent, AssistantMessage, Message, StopReason, TextContent,
    ThinkingLevel, ToolCall, Usage, now_millis,
};
use nomic_core::{Agent, AgentEvent, AgentHooks, BeforeToolCall, DynTool, ToolCallDecision};
use support::*;
use tokio_util::sync::CancellationToken;

// ── 测试 ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn text_only_prompt_single_turn() {
    let provider = MockProvider::new(vec![text_done("hello")]);
    let (mut agent, rx) = make_agent(provider, vec![DynTool::new(EchoTool)]);

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    let events = collector.await.expect("collector");

    assert_eq!(new_messages.len(), 2);
    assert!(matches!(&new_messages[0], Message::User(_)));
    assert!(matches!(&new_messages[1], Message::Assistant(_)));
    // agent_start → message_start/end(user) → turn_start → message_start(assistant)
    // → deltas → message_end → turn_end → agent_end
    assert!(matches!(events.first(), Some(AgentEvent::AgentStart)));
    assert!(matches!(events.last(), Some(AgentEvent::AgentEnd { .. })));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnStart))
            .count(),
        1
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::MessageUpdate(AssistantEvent::TextDelta { .. })
    )));
}

#[tokio::test]
async fn message_end_and_agent_end_carry_authoritative_context_tokens() {
    // 锚点规则只在 core 定义：事件携带的 context_tokens 即 estimate_context_tokens
    let mut script = text_done("ok");
    let Some(AssistantEvent::Done { message }) = script.last_mut() else {
        panic!("text_done ends with Done");
    };
    message.usage = Usage {
        total_tokens: 1_000,
        ..Usage::default()
    };
    let provider = MockProvider::new(vec![script]);
    let (mut agent, rx) = make_agent(provider, vec![]);

    let collector = tokio::spawn(collect_events(rx));
    agent
        .prompt("aaaaaaaa", CancellationToken::new())
        .await
        .expect("prompt");
    let events = collector.await.expect("collector");

    // user 落史后：尚无 usage 锚点，按 chars/4 估算（8 chars → 2）
    let user_end = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::MessageEnd {
                message,
                context_tokens,
            } if matches!(message.as_ref(), Message::User(_)) => Some(*context_tokens),
            _ => None,
        })
        .expect("user message end");
    assert_eq!(user_end, 2);

    // assistant 落史后：usage 锚点（1000，无尾部）即权威值
    let assistant_end = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::MessageEnd {
                message,
                context_tokens,
            } if matches!(message.as_ref(), Message::Assistant(_)) => Some(*context_tokens),
            _ => None,
        })
        .expect("assistant message end");
    assert_eq!(assistant_end, 1_000);

    // job 完成事件携带同一口径的权威值
    let Some(AgentEvent::AgentEnd { context_tokens, .. }) = events.last() else {
        panic!("agent end event");
    };
    assert_eq!(*context_tokens, 1_000);
}

#[tokio::test]
async fn prompt_with_images_builds_blocks_message() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (mut agent, rx) = make_agent(provider, vec![]);

    let collector = tokio::spawn(collect_events(rx));
    let images = vec![nomic_ai::ImageContent {
        data: "aGVsbG8=".to_string(),
        mime_type: "image/png".to_string(),
    }];
    let new_messages = agent
        .prompt_with_images("描述这张图", &images, CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    // user 消息为 图片块在前、文本块在后 的内容块列表，且随历史持久化
    let Message::User(user) = &new_messages[0] else {
        panic!("first message must be user");
    };
    assert_eq!(
        user.content,
        nomic_ai::UserMessageContent::Blocks(vec![
            nomic_ai::UserContent::Image(images[0].clone()),
            nomic_ai::UserContent::Text(TextContent {
                text: "描述这张图".to_string(),
                text_signature: None,
            }),
        ])
    );
    assert_eq!(agent.messages()[0], new_messages[0]);
}

#[tokio::test]
async fn set_reasoning_updates_subsequent_stream_options() {
    let provider = MockProvider::new(vec![text_done("one"), text_done("two"), text_done("three")]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![]);
    assert_eq!(agent.reasoning(), None);

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    agent.set_reasoning(Some(ThinkingLevel::High));
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    agent.set_reasoning(None);
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    assert_eq!(
        provider.reasonings(),
        vec![None, Some(ThinkingLevel::High), None]
    );
}

/// 跨 provider 的 `/models` 运行时切换：后续请求走新 provider，且 stream
/// options 的 api_key 一并替换；旧 provider 不再收到请求。
#[tokio::test]
async fn set_provider_reroutes_subsequent_requests_with_new_api_key() {
    let old = MockProvider::new(vec![]);
    let new = MockProvider::new(vec![text_done("from new provider")]);
    let (mut agent, _rx) = make_agent(old.clone(), vec![]);

    agent.set_provider(new.clone(), Some("sk-new".to_string()));
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    assert!(old.api_keys().is_empty(), "旧 provider 不再收到请求");
    assert_eq!(new.api_keys(), vec![Some("sk-new".to_string())]);
}

#[tokio::test]
async fn prompt_with_empty_images_stays_plain_text() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (mut agent, _rx) = make_agent(provider, vec![]);

    let new_messages = agent
        .prompt_with_images("hi", &[], CancellationToken::new())
        .await
        .expect("prompt");

    let Message::User(user) = &new_messages[0] else {
        panic!("first message must be user");
    };
    assert_eq!(
        user.content,
        nomic_ai::UserMessageContent::Text("hi".to_string())
    );
}

#[tokio::test]
async fn resume_with_seeded_history() {
    let provider = MockProvider::new(vec![text_done("world")]);
    let history = vec![
        Message::User(nomic_ai::UserMessage {
            content: nomic_ai::UserMessageContent::Text("old question".to_string()),
            timestamp: now_millis(),
        }),
        Message::Assistant(assistant_message(
            vec![AssistantContent::Text(TextContent {
                text: "old answer".to_string(),
                text_signature: None,
            })],
            StopReason::Stop,
        )),
    ];
    let (mut agent, rx) = Agent::builder()
        .model(model())
        .provider(provider.clone())
        .system_prompt("test system prompt")
        .tools(vec![DynTool::new(EchoTool)])
        .messages(history.clone())
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build();

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    // 返回值只含本次新增；完整历史 = 种子 + 新增
    assert_eq!(new_messages.len(), 2);
    let all = agent.messages();
    assert_eq!(all.len(), history.len() + 2);
    assert_eq!(&all[..history.len()], history.as_slice());
    // provider 收到的上下文 = 种子历史 + 新 user 消息
    assert_eq!(provider.context_lens(), vec![history.len() + 1]);
}

#[tokio::test]
async fn clear_messages_resets_context() {
    let provider = MockProvider::new(vec![text_done("hello"), text_done("world")]);
    let (mut agent, rx) = make_agent(provider.clone(), vec![DynTool::new(EchoTool)]);

    let collector = tokio::spawn(collect_events(rx));
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");
    assert_eq!(agent.messages().len(), 2);

    agent.clear_messages();
    assert!(agent.messages().is_empty());

    // 新一轮：provider 上下文只含新 user 消息，不带清空前的历史。
    // 事件接收端已随第一个 collector 关闭，emit 静默丢弃，不影响 loop。
    agent
        .prompt("again", CancellationToken::new())
        .await
        .expect("prompt");
    assert_eq!(provider.context_lens(), vec![1, 1]);
}

#[tokio::test]
async fn restore_messages_replaces_context_without_events() {
    let provider = MockProvider::new(vec![text_done("hello"), text_done("world")]);
    let (mut agent, mut rx) = make_agent(provider.clone(), vec![]);

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    assert_eq!(agent.messages().len(), 2);
    // 清掉首轮事件，隔离观察 restore 本身
    while rx.try_recv().is_ok() {}

    // session resume 语义：整体替换历史；静默，不发出事件
    // （历史已在来源 session 渲染/落库，重放会造成交互端重复渲染与重复落库）
    let restored = vec![Message::User(nomic_ai::UserMessage {
        content: nomic_ai::UserMessageContent::Text("old".to_string()),
        timestamp: now_millis(),
    })];
    agent.restore_messages(restored.clone());
    assert_eq!(agent.messages(), restored.as_slice());
    assert!(rx.try_recv().is_err(), "restore 不应发出任何事件");

    // 新一轮：provider 上下文 = 恢复的历史 + 新 user 消息
    agent
        .prompt("again", CancellationToken::new())
        .await
        .expect("prompt");
    assert_eq!(provider.context_lens(), vec![1, 2]);
}

#[tokio::test]
async fn inject_user_message_joins_history_and_emits_events() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (mut agent, rx) = make_agent(provider.clone(), vec![]);

    let collector = tokio::spawn(collect_events(rx));
    agent.inject_user_message("<active_skill name=\"demo\">body</active_skill>");
    assert_eq!(agent.messages().len(), 1);
    assert!(matches!(agent.messages()[0], Message::User(_)));

    // 后续 prompt 的上下文 = 注入消息 + 本次 user 消息
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    let events = collector.await.expect("collector");
    assert_eq!(provider.context_lens(), vec![2]);
    // 注入消息的事件先于 AgentStart 发出（驱动端不启动新 run）
    assert!(matches!(events[0], AgentEvent::MessageStart(_)));
    assert!(matches!(events[1], AgentEvent::MessageEnd { .. }));
    assert!(matches!(events[2], AgentEvent::AgentStart));
}

#[tokio::test]
async fn tool_call_then_text_two_turns() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "from tool"})),
        text_done("done"),
    ]);
    let (mut agent, rx) = make_agent(provider, vec![DynTool::new(EchoTool)]);

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    let events = collector.await.expect("collector");

    // user + assistant(tool call) + toolResult + assistant(text)
    assert_eq!(new_messages.len(), 4);
    let Message::ToolResult(result) = &new_messages[2] else {
        panic!("expected tool result")
    };
    assert_eq!(result.tool_call_id, "c1");
    assert!(!result.is_error);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnStart))
            .count(),
        2
    );
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolExecutionEnd { tool_name, is_error: false, .. } if tool_name == "echo"
    )));
}

#[tokio::test]
async fn provider_error_ends_loop_with_error_message() {
    let provider = MockProvider::new(vec![vec![
        AssistantEvent::Start,
        AssistantEvent::Error {
            message: Box::new(AssistantMessage {
                stop_reason: StopReason::Error,
                error_message: Some("boom".to_string()),
                ..assistant_message(vec![], StopReason::Error)
            }),
        },
    ]]);
    let (mut agent, rx) = make_agent(provider, vec![]);

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    let Message::Assistant(message) = &new_messages[1] else {
        panic!("expected assistant")
    };
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(message.error_message.as_deref(), Some("boom"));
}

#[tokio::test]
async fn pre_stream_error_emits_paired_message_start_and_end() {
    // 流建立前失败（如重试耗尽）：provider 只发 Error 终止事件、不发 Start
    let provider = MockProvider::new(vec![vec![AssistantEvent::Error {
        message: Box::new(AssistantMessage {
            stop_reason: StopReason::Error,
            error_message: Some("connection refused".to_string()),
            ..assistant_message(vec![], StopReason::Error)
        }),
    }]]);
    let (mut agent, rx) = make_agent(provider, vec![]);

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    let events = collector.await.expect("collector");

    let Message::Assistant(message) = &new_messages[1] else {
        panic!("expected assistant")
    };
    assert_eq!(message.stop_reason, StopReason::Error);

    // 未收到 Start 也必须补发 MessageStart，保证与 MessageEnd 配对
    let sequence: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageStart(m) if matches!(m.as_ref(), Message::Assistant(_)) => {
                Some("start")
            }
            AgentEvent::MessageEnd { message, .. }
                if matches!(message.as_ref(), Message::Assistant(_)) =>
            {
                Some("end")
            }
            _ => None,
        })
        .collect();
    assert_eq!(sequence, ["start", "end"]);
}

#[tokio::test]
async fn continue_after_error_drops_failed_message_and_reruns() {
    let provider = MockProvider::new(vec![
        error_done(StopReason::Error, "boom"),
        text_done("recovered"),
    ]);
    let (mut agent, mut rx) = make_agent(provider.clone(), vec![]);

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    assert_eq!(agent.messages().len(), 2);
    // 清掉首轮事件，隔离观察 continue 本身发出的事件
    while rx.try_recv().is_ok() {}

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .continue_run(CancellationToken::new())
        .await
        .expect("continue")
        .expect("continued");
    let events = collector.await.expect("collector");

    // 不重新注入 user 消息：本次新增只有 assistant 响应；
    // 失败消息已从历史弹出，provider 上下文只剩原 user 消息
    assert_eq!(new_messages.len(), 1);
    let all = agent.messages();
    assert_eq!(all.len(), 2);
    assert!(matches!(&all[0], Message::User(_)));
    assert!(matches!(&all[1], Message::Assistant(m) if m.stop_reason == StopReason::Stop));
    assert_eq!(provider.context_lens(), vec![1, 1]);
    // 续跑复用 prompt 的运行事件边界
    assert!(matches!(events.first(), Some(AgentEvent::AgentStart)));
    assert!(matches!(events.last(), Some(AgentEvent::AgentEnd { .. })));
}

#[tokio::test]
async fn continue_after_abort_drops_aborted_message_and_reruns() {
    let provider = MockProvider::new(vec![
        error_done(StopReason::Aborted, "cancelled"),
        text_done("recovered"),
    ]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![]);

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    let continued = agent
        .continue_run(CancellationToken::new())
        .await
        .expect("continue")
        .expect("continued");

    assert_eq!(continued.len(), 1);
    assert_eq!(agent.messages().len(), 2);
    assert_eq!(provider.context_lens(), vec![1, 1]);
}

#[tokio::test]
async fn continue_after_successful_run_returns_none() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![]);

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    let outcome = agent
        .continue_run(CancellationToken::new())
        .await
        .expect("continue");

    // 最近一轮已成功：尾部是 assistant，无可续跑消息，历史不变，不消耗 provider 脚本
    assert!(outcome.is_none());
    assert_eq!(agent.messages().len(), 2);
    assert_eq!(provider.context_lens(), vec![1]);
}

#[tokio::test]
async fn continue_on_empty_history_returns_none() {
    let provider = MockProvider::new(vec![text_done("unused")]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![]);

    let outcome = agent
        .continue_run(CancellationToken::new())
        .await
        .expect("continue");

    assert!(outcome.is_none());
    assert!(agent.messages().is_empty());
    assert!(provider.context_lens().is_empty());
}

#[tokio::test]
async fn continue_after_tool_result_reruns_without_reinjecting() {
    let provider = MockProvider::new(vec![text_done("finished the turn")]);
    let (mut agent, mut rx) = make_agent(provider.clone(), vec![]);

    // 历史以 tool result 结尾（如工具执行后被取消/终止的轮次）：续跑重发该消息
    agent.restore_messages(vec![
        Message::User(nomic_ai::UserMessage {
            content: nomic_ai::UserMessageContent::Text("run the tool".to_string()),
            timestamp: now_millis(),
        }),
        Message::ToolResult(nomic_ai::ToolResultMessage {
            tool_call_id: "c1".to_string(),
            tool_name: "echo".to_string(),
            content: vec![nomic_ai::UserContent::Text(TextContent {
                text: "tool output".to_string(),
                text_signature: None,
            })],
            details: None,
            is_error: false,
            timestamp: now_millis(),
        }),
    ]);
    while rx.try_recv().is_ok() {}

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .continue_run(CancellationToken::new())
        .await
        .expect("continue")
        .expect("continued");
    let events = collector.await.expect("collector");

    // 不重新注入消息：新增只有 assistant 响应，tool result 留在历史中作为上下文
    assert_eq!(new_messages.len(), 1);
    assert!(matches!(
        &new_messages[0],
        Message::Assistant(m) if m.stop_reason == StopReason::Stop
    ));
    assert_eq!(agent.messages().len(), 3);
    assert!(matches!(
        agent.messages().last(),
        Some(Message::Assistant(_))
    ));
    assert!(!events.iter().any(|e| matches!(
        e,
        AgentEvent::MessageStart(m) if matches!(m.as_ref(), Message::User(_))
    )));
}

#[tokio::test]
async fn length_stop_fails_all_tool_calls() {
    let mut truncated = tool_call_done("c1", "echo", serde_json::json!({"text": "x"}));
    // 把 Done 换成 Length 终止
    let call = ToolCall {
        id: "c1".to_string(),
        name: "echo".to_string(),
        arguments: serde_json::json!({"text": "x"}),
        thought_signature: None,
    };
    truncated.pop();
    truncated.push(AssistantEvent::Done {
        message: Box::new(assistant_message(
            vec![AssistantContent::ToolCall(call)],
            StopReason::Length,
        )),
    });
    let provider = MockProvider::new(vec![truncated, text_done("recovered")]);
    let (mut agent, rx) = make_agent(provider, vec![DynTool::new(EchoTool)]);

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    let Message::ToolResult(result) = &new_messages[2] else {
        panic!("expected tool result")
    };
    assert!(result.is_error);
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(
        text.text.contains("output token limit"),
        "unexpected: {}",
        text.text
    );
    // loop 继续第二 turn 并恢复
    assert!(matches!(&new_messages[3], Message::Assistant(m) if m.stop_reason == StopReason::Stop));
}

struct BlockAllHooks;

#[async_trait]
impl AgentHooks for BlockAllHooks {
    async fn before_tool_call(&self, _ctx: &BeforeToolCall<'_>) -> ToolCallDecision {
        ToolCallDecision::Block {
            reason: "blocked by policy".to_string(),
        }
    }
}

#[tokio::test]
async fn hook_block_produces_error_result_without_executing() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "x"})),
        text_done("ok"),
    ]);
    let (mut agent, rx) = Agent::builder()
        .model(model())
        .provider(provider)
        .system_prompt("sys")
        .tools(vec![DynTool::new(EchoTool)])
        .hooks(Arc::new(BlockAllHooks))
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build();

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    let Message::ToolResult(result) = &new_messages[2] else {
        panic!("expected tool result")
    };
    assert!(result.is_error);
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(text.text, "blocked by policy");
}

#[tokio::test]
async fn invalid_arguments_become_error_result() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"wrong_field": 1})),
        text_done("ok"),
    ]);
    let (mut agent, rx) = make_agent(provider, vec![DynTool::new(EchoTool)]);

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    let Message::ToolResult(result) = &new_messages[2] else {
        panic!("expected tool result")
    };
    assert!(result.is_error);
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(
        text.text.contains("invalid arguments"),
        "unexpected: {}",
        text.text
    );
}

#[tokio::test]
async fn unknown_tool_becomes_error_result() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "nonexistent", serde_json::json!({})),
        text_done("ok"),
    ]);
    let (mut agent, rx) = make_agent(provider, vec![DynTool::new(EchoTool)]);

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    let Message::ToolResult(result) = &new_messages[2] else {
        panic!("expected tool result")
    };
    assert!(result.is_error);
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.contains("not found"), "unexpected: {}", text.text);
}
