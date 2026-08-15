//! agent loop 集成测试的共享支撑（自 `agent_loop.rs` 拆出的子模块）：
//! 脚本化 mock provider 与消息构造 helper。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nomic_ai::{
    ApiKind, AssistantContent, AssistantEvent, AssistantMessage, Context, Model, Provider,
    StopReason, StreamOptions, TextContent, ThinkingLevel, ToolCall, Usage, now_millis,
};
use nomic_core::{Agent, AgentEvent, AgentTool, DynTool, ToolError, ToolResult};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

// ── 脚本化 mock provider ────────────────────────────────────────────────────

pub struct MockProvider {
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
    pub fn new(scripts: Vec<Vec<AssistantEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            context_lens: Mutex::new(Vec::new()),
            reasonings: Mutex::new(Vec::new()),
            api_keys: Mutex::new(Vec::new()),
        })
    }

    /// 各次 stream 调用收到的上下文消息数
    pub fn context_lens(&self) -> Vec<usize> {
        self.context_lens.lock().expect("lock").clone()
    }

    /// 各次 stream 调用收到的思考级别
    pub fn reasonings(&self) -> Vec<Option<ThinkingLevel>> {
        self.reasonings.lock().expect("lock").clone()
    }

    /// 各次 stream 调用收到的 api_key
    pub fn api_keys(&self) -> Vec<Option<String>> {
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

pub fn assistant_message(
    content: Vec<AssistantContent>,
    stop_reason: StopReason,
) -> AssistantMessage {
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

pub fn text_done(text: &str) -> Vec<AssistantEvent> {
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

pub fn tool_call_done(id: &str, name: &str, args: serde_json::Value) -> Vec<AssistantEvent> {
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

pub fn error_done(stop_reason: StopReason, error: &str) -> Vec<AssistantEvent> {
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

// ── 测试工具 ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
#[allow(dead_code)]
pub struct EchoParams {
    text: String,
}

pub struct EchoTool;

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

pub fn model() -> Model {
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

pub fn make_agent(
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

pub async fn collect_events(
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
