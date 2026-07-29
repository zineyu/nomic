//! agent loop 集成测试：用脚本化 mock provider 验证 loop 行为。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nomic_ai::{
    ApiKind, AssistantContent, AssistantEvent, AssistantMessage, Context, Message, Model, Provider,
    StopReason, StreamOptions, TextContent, ToolCall, Usage, now_millis,
};
use nomic_core::{
    Agent, AgentConfig, AgentEvent, AgentHooks, AgentTool, BeforeToolCall, DynTool, NoopHooks,
    ToolCallDecision, ToolError, ToolResult,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

// ── 脚本化 mock provider ────────────────────────────────────────────────────

struct MockProvider {
    /// 每次 stream 调用弹出一段事件脚本
    scripts: Mutex<VecDeque<Vec<AssistantEvent>>>,
    /// 每次 stream 调用收到的上下文消息数（验证历史注入）
    context_lens: Mutex<Vec<usize>>,
}

impl MockProvider {
    fn new(scripts: Vec<Vec<AssistantEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            context_lens: Mutex::new(Vec::new()),
        })
    }

    /// 各次 stream 调用收到的上下文消息数
    fn context_lens(&self) -> Vec<usize> {
        self.context_lens.lock().expect("lock").clone()
    }
}

impl Provider for MockProvider {
    fn stream(
        &self,
        _model: &Model,
        context: &Context,
        _options: &StreamOptions,
        _cancel: CancellationToken,
    ) -> nomic_ai::AssistantStream {
        self.context_lens
            .lock()
            .expect("lock")
            .push(context.messages.len());
        let events = self
            .scripts
            .lock()
            .expect("lock")
            .pop_front()
            .expect("no scripted response left");
        let (tx, stream) = nomic_ai::channel();
        tokio::spawn(async move {
            for event in events {
                let _ = tx.send(event);
            }
        });
        stream
    }
}

fn assistant_message(content: Vec<AssistantContent>, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        content,
        api: ApiKind::OpenAiCompletions,
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason,
        error_message: None,
        timestamp: now_millis(),
    }
}

fn text_done(text: &str) -> Vec<AssistantEvent> {
    vec![
        AssistantEvent::Start,
        AssistantEvent::TextStart { index: 0 },
        AssistantEvent::TextDelta {
            index: 0,
            delta: text.to_string(),
        },
        AssistantEvent::TextEnd { index: 0 },
        AssistantEvent::Done {
            message: Box::new(assistant_message(
                vec![AssistantContent::Text(TextContent {
                    text: text.to_string(),
                    text_signature: None,
                })],
                StopReason::Stop,
            )),
        },
    ]
}

fn tool_call_done(id: &str, name: &str, args: serde_json::Value) -> Vec<AssistantEvent> {
    let call = ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: args,
        thought_signature: None,
    };
    vec![
        AssistantEvent::Start,
        AssistantEvent::ToolCallStart { index: 0 },
        AssistantEvent::ToolCallEnd {
            index: 0,
            tool_call: call.clone(),
        },
        AssistantEvent::Done {
            message: Box::new(assistant_message(
                vec![AssistantContent::ToolCall(call)],
                StopReason::ToolUse,
            )),
        },
    ]
}

// ── 测试工具 ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)]
struct EchoParams {
    text: String,
}

struct EchoTool;

#[async_trait]
impl AgentTool for EchoTool {
    type Params = EchoParams;

    fn name(&self) -> &'static str {
        "echo"
    }

    fn label(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Echo the input text back."
    }

    async fn execute(
        &self,
        params: Self::Params,
        _cancel: CancellationToken,
        _on_update: nomic_core::ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text(params.text))
    }
}

fn model() -> Model {
    Model {
        id: "mock-model".to_string(),
        name: "mock".to_string(),
        api: ApiKind::OpenAiCompletions,
        provider: "mock".to_string(),
        base_url: "http://localhost".to_string(),
        reasoning: false,
        context_window: 128_000,
        max_tokens: 4096,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    }
}

fn make_agent(
    provider: Arc<MockProvider>,
    tools: Vec<DynTool>,
) -> (Agent, tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
    Agent::new(
        AgentConfig {
            model: model(),
            provider,
            stream_options: StreamOptions::default(),
            hooks: Arc::new(NoopHooks),
            tool_execution: nomic_core::ExecutionMode::Parallel,
        },
        tools,
        "test system prompt",
    )
}

async fn collect_events(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        let is_end = matches!(event, AgentEvent::AgentEnd { .. });
        events.push(event);
        if is_end {
            break;
        }
    }
    events
}

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
    let (mut agent, rx) = Agent::with_messages(
        AgentConfig {
            model: model(),
            provider: provider.clone(),
            stream_options: StreamOptions::default(),
            hooks: Arc::new(NoopHooks),
            tool_execution: nomic_core::ExecutionMode::Parallel,
        },
        vec![DynTool::new(EchoTool)],
        "test system prompt",
        history.clone(),
    );

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
    assert!(matches!(events[1], AgentEvent::MessageEnd(_)));
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
    let (mut agent, rx) = Agent::new(
        AgentConfig {
            model: model(),
            provider,
            stream_options: StreamOptions::default(),
            hooks: Arc::new(BlockAllHooks),
            tool_execution: nomic_core::ExecutionMode::Parallel,
        },
        vec![DynTool::new(EchoTool)],
        "sys",
    );

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
