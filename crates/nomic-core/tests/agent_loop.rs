//! agent loop 集成测试：用脚本化 mock provider 验证 loop 行为。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nomic_ai::{
    ApiKind, AssistantContent, AssistantEvent, AssistantMessage, Context, Message, Model, Provider,
    StopReason, StreamOptions, TextContent, ThinkingLevel, ToolCall, Usage, now_millis,
};
use nomic_core::{
    Agent, AgentEvent, AgentHooks, AgentTool, BeforeToolCall, DynTool, ToolCallDecision, ToolError,
    ToolResult,
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
    /// 每次 stream 调用收到的思考级别（验证 stream options 传递）
    reasonings: Mutex<Vec<Option<ThinkingLevel>>>,
    /// 每次 stream 调用收到的 api_key（验证 provider 切换后 key 一并替换）
    api_keys: Mutex<Vec<Option<String>>>,
}

impl MockProvider {
    fn new(scripts: Vec<Vec<AssistantEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            context_lens: Mutex::new(Vec::new()),
            reasonings: Mutex::new(Vec::new()),
            api_keys: Mutex::new(Vec::new()),
        })
    }

    /// 各次 stream 调用收到的上下文消息数
    fn context_lens(&self) -> Vec<usize> {
        self.context_lens.lock().expect("lock").clone()
    }

    /// 各次 stream 调用收到的思考级别
    fn reasonings(&self) -> Vec<Option<ThinkingLevel>> {
        self.reasonings.lock().expect("lock").clone()
    }

    /// 各次 stream 调用收到的 api_key
    fn api_keys(&self) -> Vec<Option<String>> {
        self.api_keys.lock().expect("lock").clone()
    }
}

impl Provider for MockProvider {
    fn stream(
        &self,
        _model: &Model,
        context: &Context,
        options: &StreamOptions,
        _cancel: CancellationToken,
    ) -> nomic_ai::AssistantStream {
        self.context_lens
            .lock()
            .expect("lock")
            .push(context.messages.len());
        self.reasonings
            .lock()
            .expect("lock")
            .push(options.reasoning);
        self.api_keys
            .lock()
            .expect("lock")
            .push(options.api_key.clone());
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
    Agent::builder()
        .model(model())
        .provider(provider)
        .system_prompt("test system prompt")
        .tools(tools)
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build()
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
            AgentEvent::MessageEnd(m) if matches!(m.as_ref(), Message::Assistant(_)) => Some("end"),
            _ => None,
        })
        .collect();
    assert_eq!(sequence, ["start", "end"]);
}

fn error_done(stop_reason: StopReason, error: &str) -> Vec<AssistantEvent> {
    vec![
        AssistantEvent::Start,
        AssistantEvent::Error {
            message: Box::new(AssistantMessage {
                stop_reason,
                error_message: Some(error.to_string()),
                ..assistant_message(vec![], stop_reason)
            }),
        },
    ]
}

#[tokio::test]
async fn retry_after_error_drops_failed_message_and_reruns() {
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
    // 清掉首轮事件，隔离观察 retry 本身发出的事件
    while rx.try_recv().is_ok() {}

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .retry(CancellationToken::new())
        .await
        .expect("retry")
        .expect("retried");
    let events = collector.await.expect("collector");

    // 不重新注入 user 消息：本次新增只有 assistant 响应；
    // 失败消息已从历史弹出，provider 上下文只剩原 user 消息
    assert_eq!(new_messages.len(), 1);
    let all = agent.messages();
    assert_eq!(all.len(), 2);
    assert!(matches!(&all[0], Message::User(_)));
    assert!(matches!(&all[1], Message::Assistant(m) if m.stop_reason == StopReason::Stop));
    assert_eq!(provider.context_lens(), vec![1, 1]);
    // 重试复用 prompt 的运行事件边界
    assert!(matches!(events.first(), Some(AgentEvent::AgentStart)));
    assert!(matches!(events.last(), Some(AgentEvent::AgentEnd { .. })));
}

#[tokio::test]
async fn retry_after_abort_drops_aborted_message_and_reruns() {
    let provider = MockProvider::new(vec![
        error_done(StopReason::Aborted, "cancelled"),
        text_done("recovered"),
    ]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![]);

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    let retried = agent
        .retry(CancellationToken::new())
        .await
        .expect("retry")
        .expect("retried");

    assert_eq!(retried.len(), 1);
    assert_eq!(agent.messages().len(), 2);
    assert_eq!(provider.context_lens(), vec![1, 1]);
}

#[tokio::test]
async fn retry_after_successful_run_returns_none() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![]);

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    let outcome = agent.retry(CancellationToken::new()).await.expect("retry");

    // 最近一轮已成功：无可重试状态，历史不变，不消耗 provider 脚本
    assert!(outcome.is_none());
    assert_eq!(agent.messages().len(), 2);
    assert_eq!(provider.context_lens(), vec![1]);
}

#[tokio::test]
async fn retry_on_empty_history_returns_none() {
    let provider = MockProvider::new(vec![text_done("unused")]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![]);

    let outcome = agent.retry(CancellationToken::new()).await.expect("retry");

    assert!(outcome.is_none());
    assert!(agent.messages().is_empty());
    assert!(provider.context_lens().is_empty());
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

// ── steering 队列（ADR-0013，pi 式运行中转向）──────────────────────────────

/// 门控工具：开始执行即通知测试，随后阻塞直到测试放行——
/// 用于在工具执行中途（run 进行中）确定性地入队 steering。
struct GateTool {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl AgentTool for GateTool {
    type Params = EchoParams;

    fn name(&self) -> &'static str {
        "gate"
    }

    fn label(&self) -> &'static str {
        "gate"
    }

    fn description(&self) -> &'static str {
        "Block until released by the test."
    }

    async fn execute(
        &self,
        _params: Self::Params,
        _cancel: CancellationToken,
        _on_update: nomic_core::ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(ToolResult::text("released"))
    }
}

/// 工具执行中途入队的 steering 在当前 turn 的工具调用完成后、下一次
/// LLM 调用前注入：作为 user 消息进入历史与本次新增，run 继续。
#[tokio::test]
async fn steering_pushed_mid_run_injected_at_turn_boundary() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "gate", serde_json::json!({"text": "x"})),
        text_done("done"),
    ]);
    let (mut agent, rx) = make_agent(
        provider.clone(),
        vec![DynTool::new(GateTool {
            started: started.clone(),
            release: release.clone(),
        })],
    );
    let steering = agent.steering_handle();

    let collector = tokio::spawn(collect_events(rx));
    let run = tokio::spawn(async move {
        agent
            .prompt("hi", CancellationToken::new())
            .await
            .expect("prompt")
    });
    // 等工具开始执行（turn 1 进入工具阶段），在工具完成前入队 steering
    started.notified().await;
    steering.push(nomic_core::SteeringMessage {
        text: "顺便把测试也补上".to_string(),
        images: Vec::new(),
    });
    release.notify_one();
    let new_messages = run.await.expect("run");
    collector.await.expect("collector");

    // 历史：user → assistant(toolcall) → toolResult → user(steering) → assistant
    let kinds: Vec<&str> = new_messages
        .iter()
        .map(|m| match m {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        })
        .collect();
    assert_eq!(
        kinds,
        ["user", "assistant", "toolResult", "user", "assistant"]
    );
    let Message::User(steered) = &new_messages[3] else {
        panic!("第四条应为 steering 注入的 user 消息");
    };
    assert_eq!(
        steered.content,
        nomic_ai::UserMessageContent::Text("顺便把测试也补上".to_string())
    );
    // 第二次 LLM 调用的上下文已含 steering 消息
    assert_eq!(provider.context_lens(), vec![1, 4]);
    assert!(steering.is_empty(), "注入后队列已排空");
}

/// one-at-a-time：每个完成的 turn 投递一条；模型无工具调用但队列未
/// 清空时 run 不结束，继续注入续行直至排空。
#[tokio::test]
async fn steering_one_at_a_time_keeps_run_alive_until_drained() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "x"})),
        text_done("a"),
        text_done("b"),
    ]);
    let (mut agent, rx) = make_agent(provider.clone(), vec![DynTool::new(EchoTool)]);
    let steering = agent.steering_handle();
    steering.push(nomic_core::SteeringMessage {
        text: "第一条转向".to_string(),
        images: Vec::new(),
    });
    steering.push(nomic_core::SteeringMessage {
        text: "第二条转向".to_string(),
        images: Vec::new(),
    });

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    // user → assistant(toolcall) → toolResult → u1 → assistant(a) → u2 → assistant(b)
    assert_eq!(new_messages.len(), 7);
    assert_eq!(agent.messages().len(), 7);
    let Message::User(first) = &new_messages[3] else {
        panic!("第四条应为第一条 steering");
    };
    let Message::User(second) = &new_messages[5] else {
        panic!("第六条应为第二条 steering");
    };
    assert_eq!(
        first.content,
        nomic_ai::UserMessageContent::Text("第一条转向".to_string())
    );
    assert_eq!(
        second.content,
        nomic_ai::UserMessageContent::Text("第二条转向".to_string())
    );
    // 三次 LLM 调用：1（初始）→ 4（+toolcall/toolResult/u1）→ 6（+assistant/u2）
    assert_eq!(provider.context_lens(), vec![1, 4, 6]);
    assert!(steering.is_empty());
}

/// 携带图片附件的 steering：与 prompt 附件同一口径，图片块在前、文本块在后。
#[tokio::test]
async fn steering_with_images_builds_blocks_message() {
    let provider = MockProvider::new(vec![text_done("t1"), text_done("t2")]);
    let (mut agent, _rx) = make_agent(provider, vec![]);
    let steering = agent.steering_handle();
    let image = nomic_ai::ImageContent {
        data: "aGVsbG8=".to_string(),
        mime_type: "image/png".to_string(),
    };
    steering.push(nomic_core::SteeringMessage {
        text: "看这张图".to_string(),
        images: vec![image.clone()],
    });

    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    // 首轮无工具调用，但 steering 未清空 → 注入续行：user, asst, u(steered), asst
    assert_eq!(new_messages.len(), 4);
    let Message::User(steered) = &new_messages[2] else {
        panic!("第三条应为 steering 注入的 user 消息");
    };
    assert_eq!(
        steered.content,
        nomic_ai::UserMessageContent::Blocks(vec![
            nomic_ai::UserContent::Image(image),
            nomic_ai::UserContent::Text(TextContent {
                text: "看这张图".to_string(),
                text_signature: None,
            }),
        ])
    );
}

/// 响应以 Error 收尾时不注入 steering，队列保留（失败恢复由用户主导）。
#[tokio::test]
async fn error_turn_does_not_drain_steering() {
    let provider = MockProvider::new(vec![error_done(StopReason::Error, "boom")]);
    let (mut agent, _rx) = make_agent(provider, vec![]);
    let steering = agent.steering_handle();
    steering.push(nomic_core::SteeringMessage {
        text: "转向".to_string(),
        images: Vec::new(),
    });

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    assert_eq!(steering.len(), 1, "异常收尾保留 steering 队列");
    assert_eq!(agent.messages().len(), 2, "历史不含 steering 消息");
}

/// 冻结期间（QUEUE 编辑）turn 边界不弹出 steering；run 可正常结束，
/// 解冻后队列内容不变。
#[tokio::test]
async fn frozen_steering_is_not_injected() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "x"})),
        text_done("done"),
    ]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![DynTool::new(EchoTool)]);
    let steering = agent.steering_handle();
    steering.push(nomic_core::SteeringMessage {
        text: "转向".to_string(),
        images: Vec::new(),
    });
    steering.freeze();

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    // 冻结：两次 LLM 调用（toolcall turn + 收尾 turn），无 steering 注入
    assert_eq!(provider.context_lens(), vec![1, 3]);
    assert_eq!(agent.messages().len(), 4);
    assert_eq!(steering.len(), 1);
    steering.unfreeze();
    let popped = steering.pop_front().expect("解冻后可弹出");
    assert_eq!(popped.text, "转向");
}

/// 共享句柄：builder 注入的队列与 agent 内部是同一份（交互端直推语义）。
#[tokio::test]
async fn builder_accepts_shared_steering_queue() {
    let shared = nomic_core::SteeringQueue::new();
    let provider = MockProvider::new(vec![text_done("t1"), text_done("t2")]);
    let (mut agent, _rx) = Agent::builder()
        .model(model())
        .provider(provider)
        .system_prompt("test system prompt")
        .steering_queue(shared.clone())
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build();

    shared.push(nomic_core::SteeringMessage {
        text: "外部入队".to_string(),
        images: Vec::new(),
    });
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    assert!(shared.is_empty(), "agent 消费的是同一份队列");
}
