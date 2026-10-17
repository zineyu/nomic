//! rig-core Chat Completions 适配器。
//!
//! 把 nomic 领域消息与工具声明映射为 rig 的 completion 请求，把
//! rig 的流式回复映射回 [`LlmEvent`]。provider 的原始工具调用 id
//! 不作为领域 id：领域侧为每次调用生成 [`ToolCallId`]，回放时以同
//! 一 id 作为 wire 上的 call id 与工具结果配对。

use futures_util::StreamExt;
use nomic_core::{
    LlmAdapter, LlmError, LlmEvent, LlmEventStream, Message, SessionId, ToolCall, ToolCallId,
    ToolName, ToolResult, ToolSpec,
};
use rig_core::completion::{CompletionRequest, ToolDefinition};
use rig_core::message::{
    AssistantContent, AssistantMessage, CallId, ToolCall as RigToolCall, ToolFunction,
    ToolName as RigToolName, ToolResult as RigToolResult, ToolResultContent, UserContent,
};
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::Chat;
use rig_core::streaming::{Item, StreamEvent};
use rig_core::{Model, ProviderError};
use std::collections::HashMap;

use crate::config::ProviderConfig;

/// 未配置 provider 时给出的修正指引。
const UNCONFIGURED_HINT: &str = "设置 NOMIC_API_KEY、NOMIC_MODEL（可选 NOMIC_BASE_URL），\
     或写入 ~/.config/nomic/config.toml 的 [provider] 段后重启 server";

/// rig-core 适配器：OpenAI 兼容端点，或显式的未配置状态。
#[derive(Clone, Debug)]
pub struct RigAdapter {
    backend: Backend,
}

#[derive(Clone, Debug)]
enum Backend {
    Configured(Box<Model<Chat>>),
    Unconfigured(String),
}

impl RigAdapter {
    /// 以 provider 配置构造 OpenAI 兼容端点适配器。
    pub fn openai(config: &ProviderConfig) -> Self {
        let mut openai = OpenAIConfig::new(config.api_key().expose().to_owned());
        if let Some(base_url) = config.base_url() {
            openai = openai.with_base_url(base_url);
        }
        Self {
            backend: Backend::Configured(Box::new(openai.client().chat(config.model()))),
        }
    }

    /// 直接以 rig 模型构造适配器（测试 seam 与自定义 transport 使用）。
    pub(crate) fn from_model(model: Model<Chat>) -> Self {
        Self {
            backend: Backend::Configured(Box::new(model)),
        }
    }

    /// 未配置 provider 的适配器：请求显式失败，并给出修正指引。
    pub fn unconfigured() -> Self {
        Self {
            backend: Backend::Unconfigured(UNCONFIGURED_HINT.to_owned()),
        }
    }
}

impl LlmAdapter for RigAdapter {
    async fn respond(
        &self,
        _session: SessionId,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<LlmEventStream, LlmError> {
        let Backend::Configured(model) = &self.backend else {
            let Backend::Unconfigured(hint) = &self.backend else {
                unreachable!("Backend 只有两种状态")
            };
            return Err(LlmError::Unconfigured(hint.clone()));
        };
        let request = completion_request(messages, tools)?;
        let stream = model.stream(request).map_err(open_error)?;
        let events = stream.filter_map(|item| async move {
            match item {
                Ok(item) => llm_event(item).map(Ok),
                Err(error) => Some(Err(provider_error(error))),
            }
        });
        let stream: LlmEventStream = Box::pin(events);
        Ok(stream)
    }
}

/// 组装 completion 请求：完整历史 + 工具声明。
fn completion_request(
    messages: &[Message],
    tools: &[ToolSpec],
) -> Result<CompletionRequest, LlmError> {
    let history = chat_history(messages)?;
    let prompt = history
        .last()
        .cloned()
        .ok_or_else(|| LlmError::Provider("消息历史为空，无法请求模型".into()))?;
    let mut request = CompletionRequest::new(prompt);
    request.chat_history = history;
    request.tools = tools
        .iter()
        .map(|spec| {
            Ok(ToolDefinition::new(
                rig_tool_name(&spec.name)?,
                spec.description.clone(),
                spec.input_schema.clone(),
            ))
        })
        .collect::<Result<_, LlmError>>()?;
    Ok(request)
}

/// 领域消息历史映射为 rig 消息。
///
/// 无文本也无工具调用的助手消息不携带信息，不发送。工具结果按调用
/// id 查回请求它的助手消息里的工具名。
fn chat_history(messages: &[Message]) -> Result<Vec<rig_core::message::Message>, LlmError> {
    let mut calls: HashMap<ToolCallId, ToolName> = HashMap::new();
    let mut history = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            Message::System { text, .. } => {
                history.push(rig_core::message::Message::system(text.clone()));
            }
            Message::User { text, .. } => {
                history.push(rig_core::message::Message::user(text.clone()));
            }
            Message::Assistant {
                text, tool_calls, ..
            } => {
                let mut content = Vec::with_capacity(tool_calls.len() + 1);
                if !text.is_empty() {
                    content.push(AssistantContent::text(text.clone()));
                }
                for call in tool_calls {
                    calls.insert(call.id, call.tool.clone());
                    content.push(AssistantContent::ToolCall(rig_call(call)?));
                }
                if content.is_empty() {
                    continue;
                }
                history.push(rig_core::message::Message::Assistant(
                    AssistantMessage::new(content),
                ));
            }
            Message::Tool {
                call_id, result, ..
            } => {
                let name = calls.get(call_id).ok_or_else(|| {
                    LlmError::Provider(format!("工具结果找不到对应的调用: {call_id}"))
                })?;
                history.push(tool_result_message(call_id, name, result)?);
            }
        }
    }
    Ok(history)
}

/// 工具结果消息：id 与工具名回填，失败标记由结果层决定。
fn tool_result_message(
    call_id: &ToolCallId,
    name: &ToolName,
    result: &ToolResult,
) -> Result<rig_core::message::Message, LlmError> {
    let (is_error, text) = match result {
        ToolResult::Ok { output } => (false, output),
        ToolResult::Error { message } => (true, message),
        ToolResult::Rejected { reason } => (true, reason),
    };
    Ok(rig_core::message::Message::User {
        content: vec![UserContent::ToolResult(RigToolResult {
            call: CallId::from_wire(call_id.to_string()),
            name: rig_tool_name(name)?,
            content: vec![ToolResultContent::text(text.clone())],
            is_error,
        })],
    })
}

/// 领域工具调用映射为 rig 调用。
fn rig_call(call: &ToolCall) -> Result<RigToolCall, LlmError> {
    Ok(RigToolCall::new(
        CallId::from_wire(call.id.to_string()),
        ToolFunction::new(rig_tool_name(&call.tool)?, call.arguments.clone()),
    ))
}

/// 领域工具名映射为 rig 工具名。
fn rig_tool_name(name: &ToolName) -> Result<RigToolName, LlmError> {
    RigToolName::new(name.as_str()).map_err(|_| LlmError::Provider(format!("工具名为空: {name}")))
}

/// rig 流元素映射为领域事件；无领域语义的元素（推理、分片开始、
/// 未建模载荷）丢弃。
fn llm_event(item: Item<StreamEvent>) -> Option<LlmEvent> {
    match item {
        Item::Event(StreamEvent::Text { text, .. }) => Some(LlmEvent::Token(text)),
        Item::Event(StreamEvent::End {
            content: AssistantContent::ToolCall(call),
            ..
        }) => Some(LlmEvent::ToolCallRequested(domain_call(call))),
        Item::Event(
            StreamEvent::Start { .. }
            | StreamEvent::Reasoning { .. }
            | StreamEvent::Arguments { .. }
            | StreamEvent::End { .. },
        )
        | Item::Unknown(_) => None,
    }
}

/// rig 工具调用映射为领域调用：领域 id 重新生成，参数保持模型给出的
/// JSON；非法参数保留原文，由工具的 schema 校验显式拒绝。
fn domain_call(call: RigToolCall) -> ToolCall {
    let arguments = match call.function.invalid_arguments {
        Some(raw) => serde_json::Value::String(raw),
        None => serde_json::Value::Object(call.function.arguments),
    };
    ToolCall {
        id: ToolCallId::new(),
        tool: ToolName::new(call.function.name.as_str()),
        arguments,
    }
}

/// 建立请求即失败的错误映射。
fn open_error(error: ProviderError) -> LlmError {
    provider_error(error)
}

/// provider 错误映射：无 provider 应答的传输失败可重试，其余是
/// provider 拒绝或协议错误。
fn provider_error(error: ProviderError) -> LlmError {
    match error {
        ProviderError::Http(error) => LlmError::Transport(error.to_string()),
        ProviderError::Truncated => LlmError::Transport("provider 回复在结束前中断".to_owned()),
        // provider 明确答复的重试判定沿用 rig 的结论：5xx / 429 等
        // 瞬时失败可重试，其余（认证、请求错误、拒绝）不可重试
        ProviderError::ProviderResponse(response) if response.is_retryable() => {
            LlmError::Transport(format!("provider 瞬时失败: {}", response.body))
        }
        other => LlmError::Provider(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use nomic_core::{MessageId, SessionId, ToolCall, ToolResult, ToolSpec};
    use rig_core::providers::openai::OpenAIConfig;
    use rig_core::test_utils::{MockHttpResponse, SequencedHttpClient};
    use serde_json::json;

    use super::*;
    use crate::sse::{self, ScriptedEvent, ScriptedTurn};
    use crate::{ApiKey, ProviderConfig};

    const KEY: &str = "sk-test-secret";

    fn adapter(responses: Vec<MockHttpResponse>) -> (RigAdapter, SequencedHttpClient) {
        let http = SequencedHttpClient::new(responses);
        let config = OpenAIConfig::new(KEY).with_base_url("http://test.invalid/v1");
        (
            RigAdapter::from_model(config.connect(http.clone()).chat("test-model")),
            http,
        )
    }

    fn scripted(turn: &ScriptedTurn) -> (RigAdapter, SequencedHttpClient) {
        adapter(vec![sse::response(turn)])
    }

    fn system() -> Message {
        Message::System {
            id: MessageId::new(),
            text: "内置提示".into(),
        }
    }

    fn user(text: &str) -> Message {
        Message::User {
            id: MessageId::new(),
            text: text.into(),
        }
    }

    fn tool_spec() -> ToolSpec {
        ToolSpec {
            name: ToolName::new("read"),
            description: "读取文件".into(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            output_schema: json!({"type": "string"}),
            replay_safe: true,
        }
    }

    fn sent_body(http: &SequencedHttpClient) -> serde_json::Value {
        let requests = http.requests();
        let body = &requests.last().expect("请求已发出").body;
        serde_json::from_slice(body).expect("请求体是 JSON")
    }

    /// Chat wire 的文本内容：字符串或文本分片数组。
    fn text_of(message: &serde_json::Value) -> String {
        match &message["content"] {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect(),
            other => panic!("非预期内容: {other}"),
        }
    }

    #[tokio::test]
    async fn 文本流映射为_token_事件且请求携带系统提示与工具声明() {
        let (adapter, http) = scripted(&ScriptedTurn::reply(["你", "好"]));
        let messages = [system(), user("任务")];
        let tools = [tool_spec()];
        let stream = adapter
            .respond(SessionId::new(), &messages, &tools)
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        let tokens: Vec<_> = events
            .into_iter()
            .map(|event| match event {
                Ok(LlmEvent::Token(text)) => text,
                other => panic!("必须是文本增量: {other:?}"),
            })
            .collect();
        assert_eq!(tokens, ["你", "好"]);

        let body = sent_body(&http);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(text_of(&body["messages"][0]), "内置提示");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(text_of(&body["messages"][1]), "任务");
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        assert_eq!(body["stream"], true);
    }

    #[tokio::test]
    async fn 工具调用帧映射为领域调用_领域_id_为_uuid() {
        let turn = ScriptedTurn::Reply(vec![ScriptedEvent::tool_call(
            "call_provider",
            "read",
            json!({"path": "src/lib.rs"}),
        )]);
        let (adapter, _http) = scripted(&turn);
        let messages = [system(), user("任务")];
        let stream = adapter
            .respond(SessionId::new(), &messages, &[])
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        let [Ok(LlmEvent::ToolCallRequested(call))] = &events[..] else {
            panic!("必须恰好一个工具调用事件: {events:?}");
        };
        assert_eq!(call.tool.as_str(), "read");
        assert_eq!(call.arguments, json!({"path": "src/lib.rs"}));
        assert_ne!(
            call.id.to_string(),
            "call_provider",
            "领域 id 不复用 provider 的 wire id"
        );
        assert_eq!(call.id.to_string().len(), 36, "领域 id 是 UUID");
    }

    #[tokio::test]
    async fn 工具结果回灌为带同一调用_id_的_tool_消息() {
        let call = ToolCall {
            id: ToolCallId::new(),
            tool: ToolName::new("read"),
            arguments: json!({"path": "a.txt"}),
        };
        let messages = [
            system(),
            user("任务"),
            Message::Assistant {
                id: MessageId::new(),
                text: "读一下".into(),
                tool_calls: vec![call.clone()],
            },
            Message::Tool {
                id: MessageId::new(),
                call_id: call.id,
                result: ToolResult::error("文件不存在"),
            },
        ];
        let (adapter, http) = scripted(&ScriptedTurn::reply(["完成"]));
        let tools = [tool_spec()];
        let stream = adapter
            .respond(SessionId::new(), &messages, &tools)
            .await
            .unwrap();
        let _: Vec<_> = stream.collect().await;

        let body = sent_body(&http);
        assert_eq!(body["messages"][2]["role"], "assistant");
        assert_eq!(text_of(&body["messages"][2]), "读一下");
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["id"],
            call.id.to_string()
        );
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["messages"][3]["tool_call_id"], call.id.to_string());
        assert_eq!(text_of(&body["messages"][3]), "文件不存在");
    }

    #[tokio::test]
    async fn 空文本空调用的助手消息不发送() {
        let messages = [
            system(),
            user("任务"),
            Message::Assistant {
                id: MessageId::new(),
                text: String::new(),
                tool_calls: Vec::new(),
            },
            user("继续"),
        ];
        let history = chat_history(&messages).unwrap();
        assert_eq!(history.len(), 3, "空助手消息不进入请求历史");
        assert!(
            history
                .iter()
                .all(|message| !matches!(message, rig_core::message::Message::Assistant(_))),
            "请求历史不得包含助手消息: {history:?}"
        );
    }

    #[tokio::test]
    async fn http_失败映射为传输错误() {
        let (adapter, _http) = adapter(vec![MockHttpResponse::error(
            rig_core::http_client::StatusCode::INTERNAL_SERVER_ERROR,
            "boom",
        )]);
        let messages = [system(), user("任务")];
        let failure = match adapter.respond(SessionId::new(), &messages, &[]).await {
            Ok(mut stream) => {
                let mut failure = None;
                while let Some(item) = stream.next().await {
                    if let Err(error) = item {
                        failure = Some(error);
                    }
                }
                failure.expect("流必须给出错误")
            }
            Err(error) => error,
        };
        match failure {
            LlmError::Transport(message) => {
                assert!(message.contains("boom"), "实际: {message}");
            }
            other => panic!("HTTP 失败必须是传输错误: {other:?}"),
        }
    }

    #[tokio::test]
    async fn 空消息历史显式失败() {
        let (adapter, _http) = scripted(&ScriptedTurn::reply(["好"]));
        let result = adapter.respond(SessionId::new(), &[], &[]).await;
        assert!(matches!(result, Err(LlmError::Provider(_))));
    }

    #[test]
    fn 适配器_debug_不含密钥() {
        let config = ProviderConfig::new(
            Some("http://test.invalid/v1".into()),
            ApiKey::parse(KEY).unwrap(),
            "test-model",
        )
        .unwrap();
        let adapter = RigAdapter::openai(&config);
        let rendered = format!("{adapter:?}");
        assert!(!rendered.contains(KEY), "实际: {rendered}");
    }

    #[test]
    fn 未配置适配器请求显式失败并给出指引() {
        let adapter = RigAdapter::unconfigured();
        let rendered = format!("{adapter:?}");
        assert!(!rendered.contains(KEY));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = runtime
            .block_on(adapter.respond(SessionId::new(), &[user("任务")], &[]))
            .err()
            .expect("未配置必须失败");
        match error {
            LlmError::Unconfigured(hint) => {
                assert!(hint.contains("NOMIC_API_KEY"), "实际: {hint}");
            }
            other => panic!("必须为未配置错误: {other:?}"),
        }
    }
}
