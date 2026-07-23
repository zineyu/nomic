//! Anthropic Messages API provider。
//!
//! 流式事件映射忠实复刻 pi-ai 的 `anthropic-messages.ts`：
//! `message_start`（初始 usage）→ 内容块 `start/delta/stop` →
//! `message_delta`（`stop_reason` + 最终 usage）→ `Done`。
//! M1 未实现：prompt caching、OAuth 身份、deferred tools（见 ADR-0001）。

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::stream::{AssistantStream, Provider, StreamOptions, channel};
use crate::types::{
    AssistantContent, AssistantMessage, Context, Message, Model, StopReason, TextContent,
    ThinkingContent, ThinkingLevel, ToolCall, UserContent,
};
use crate::{AssistantEvent, now_millis};

/// Anthropic Messages API provider。
pub struct AnthropicProvider {
    client: reqwest::Client,
    /// 缺省 API key（`StreamOptions.api_key` 优先）
    api_key: Option<String>,
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicProvider").finish_non_exhaustive()
    }
}

impl AnthropicProvider {
    /// 创建 provider；`api_key` 为 `None` 时每次请求回退到 `ANTHROPIC_API_KEY` 环境变量。
    pub fn new(api_key: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");
        Self { client, api_key }
    }

    fn resolve_api_key(&self, options: &StreamOptions) -> Result<String, String> {
        if let Some(key) = &options.api_key {
            return Ok(key.clone());
        }
        if let Some(key) = &self.api_key {
            return Ok(key.clone());
        }
        std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            "no API key: pass StreamOptions.api_key, provider key, or set ANTHROPIC_API_KEY"
                .to_string()
        })
    }
}

impl Provider for AnthropicProvider {
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
        cancel: CancellationToken,
    ) -> AssistantStream {
        let (tx, stream) = channel();
        let mut output = empty_output(model);
        let client = self.client.clone();
        let api_key = self.resolve_api_key(options);
        let request = build_request(model, context, options);
        let base_url = model.base_url.clone();
        let model_for_cost = model.clone();
        let timeout_ms = options.timeout_ms;
        let cancel_for_run = cancel.clone();

        tokio::spawn(async move {
            let result = run(
                &client,
                &base_url,
                api_key,
                &request,
                timeout_ms,
                cancel_for_run,
                &mut output,
                &tx,
            )
            .await;
            match result {
                Ok(()) => {
                    model_for_cost.calculate_cost(&mut output.usage);
                    let _ = tx.send(AssistantEvent::Done {
                        message: Box::new(output),
                    });
                }
                Err(error) => {
                    model_for_cost.calculate_cost(&mut output.usage);
                    output.stop_reason = if cancel.is_cancelled() {
                        StopReason::Aborted
                    } else {
                        StopReason::Error
                    };
                    output.error_message = Some(error);
                    let _ = tx.send(AssistantEvent::Error {
                        message: Box::new(output),
                    });
                }
            }
        });

        stream
    }
}

fn empty_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api,
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        usage: crate::types::Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: now_millis(),
    }
}

// ── HTTP 请求 ────────────────────────────────────────────────────────────────

/// 各思考级别对应的预算 token 数（与 pi 的默认 `ThinkingBudgets` 一致）。
const fn thinking_budget(level: ThinkingLevel) -> u64 {
    match level {
        ThinkingLevel::Minimal => 1024,
        ThinkingLevel::Low => 2048,
        ThinkingLevel::Medium => 8192,
        ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => 16384,
    }
}

/// 构造请求体（serde_json::Value，字段省略靠调用方控制）。
fn build_request(model: &Model, context: &Context, options: &StreamOptions) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model.id,
        "messages": convert_messages(&context.messages),
        "max_tokens": options.max_tokens.unwrap_or(model.max_tokens),
        "stream": true,
    });

    if let Some(system) = &context.system_prompt {
        body["system"] = serde_json::json!([{ "type": "text", "text": system }]);
    }
    // 温度与 extended thinking 不兼容
    if let Some(temperature) = options.temperature
        && !model.reasoning
    {
        body["temperature"] = serde_json::json!(temperature);
    }
    if !context.tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            context
                .tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect(),
        );
    }
    if model.reasoning
        && let Some(level) = options.reasoning
    {
        let budget = thinking_budget(level);
        body["thinking"] = serde_json::json!({ "type": "enabled", "budget_tokens": budget });
        // thinking 开启时 max_tokens 必须大于 budget
        let max_tokens = body["max_tokens"].as_u64().unwrap_or(0);
        if max_tokens <= budget {
            body["max_tokens"] = serde_json::json!(budget + 1024);
        }
    }
    body
}

/// 消息转换（对齐 pi 的 `convertMessages`，M1 无 OAuth/cache/deferred tools）。
fn convert_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut params = Vec::new();
    let mut iter = messages.iter().peekable();

    while let Some(message) = iter.next() {
        match message {
            Message::User(user) => match &user.content {
                crate::types::UserMessageContent::Text(text) => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    params.push(serde_json::json!({ "role": "user", "content": text }));
                }
                crate::types::UserMessageContent::Blocks(blocks) => {
                    let converted: Vec<serde_json::Value> =
                        blocks.iter().filter_map(convert_user_block).collect();
                    if converted.is_empty() {
                        continue;
                    }
                    params.push(serde_json::json!({ "role": "user", "content": converted }));
                }
            },
            Message::Assistant(assistant) => {
                let blocks: Vec<serde_json::Value> = assistant
                    .content
                    .iter()
                    .filter_map(convert_assistant_block)
                    .collect();
                if blocks.is_empty() {
                    continue;
                }
                params.push(serde_json::json!({ "role": "assistant", "content": blocks }));
            }
            Message::ToolResult(_) => {
                // 合并连续的 toolResult 为一条 user 消息（Anthropic 协议要求）
                let mut tool_results = Vec::new();
                let mut current = Some(message);
                while let Some(Message::ToolResult(result)) = current {
                    tool_results.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": result.tool_call_id,
                        "content": result.content.iter().filter_map(convert_user_block).collect::<Vec<_>>(),
                        "is_error": result.is_error,
                    }));
                    current = iter.next_if(|m| matches!(m, Message::ToolResult(_)));
                }
                params.push(serde_json::json!({ "role": "user", "content": tool_results }));
            }
        }
    }
    params
}

fn convert_user_block(block: &UserContent) -> Option<serde_json::Value> {
    match block {
        UserContent::Text(text) => {
            if text.text.trim().is_empty() {
                None
            } else {
                Some(serde_json::json!({ "type": "text", "text": text.text }))
            }
        }
        UserContent::Image(image) => Some(serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "media_type": image.mime_type, "data": image.data },
        })),
    }
}

fn convert_assistant_block(block: &AssistantContent) -> Option<serde_json::Value> {
    match block {
        AssistantContent::Text(text) => {
            if text.text.trim().is_empty() {
                None
            } else {
                Some(serde_json::json!({ "type": "text", "text": text.text }))
            }
        }
        AssistantContent::Thinking(thinking) => {
            if thinking.redacted {
                return Some(serde_json::json!({
                    "type": "redacted_thinking",
                    "data": thinking.thinking_signature.clone().unwrap_or_default(),
                }));
            }
            let signature = thinking.thinking_signature.as_deref().unwrap_or("");
            if thinking.thinking.trim().is_empty() && signature.is_empty() {
                return None;
            }
            if signature.is_empty() {
                // 缺签名的 thinking（如被中止的流）降级为纯文本，否则 API 拒绝
                Some(serde_json::json!({ "type": "text", "text": thinking.thinking }))
            } else {
                Some(serde_json::json!({
                    "type": "thinking",
                    "thinking": thinking.thinking,
                    "signature": signature,
                }))
            }
        }
        AssistantContent::ToolCall(call) => Some(serde_json::json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            "input": call.arguments,
        })),
    }
}

// ── SSE 流处理 ───────────────────────────────────────────────────────────────

#[expect(clippy::too_many_arguments)]
async fn run(
    client: &reqwest::Client,
    base_url: &str,
    api_key: Result<String, String>,
    request: &serde_json::Value,
    timeout_ms: Option<u64>,
    cancel: CancellationToken,
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
) -> Result<(), String> {
    let api_key = api_key?;
    let mut builder = client
        .post(format!("{base_url}/v1/messages"))
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");
    if let Some(ms) = timeout_ms {
        builder = builder.timeout(Duration::from_millis(ms));
    }

    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err("request aborted".to_string()),
        result = builder.json(request).send() => result.map_err(|e| format!("request failed: {e}"))?,
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {body}"));
    }

    let _ = tx.send(AssistantEvent::Start);
    process_events(response.bytes_stream().eventsource(), cancel, output, tx).await
}

/// 消费 SSE 事件流并映射为 [`AssistantEvent`]；与 HTTP 解耦以便 fixture 回放测试。
async fn process_events<S, E>(
    mut sse: S,
    cancel: CancellationToken,
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
) -> Result<(), String>
where
    S: futures::Stream<Item = Result<eventsource_stream::Event, E>> + Unpin,
    E: std::fmt::Display,
{
    // 流式状态：(线上块 index, 在 output.content 中的位置, 工具调用 partial JSON 缓冲)
    let mut block_positions: Vec<BlockState> = Vec::new();
    let mut saw_message_start = false;
    let mut saw_message_stop = false;

    loop {
        let event = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err("request aborted".to_string()),
            event = sse.next() => match event {
                Some(Ok(event)) => event,
                Some(Err(e)) => return Err(format!("SSE decode failed: {e}")),
                None => break,
            },
        };
        if event.event == "ping" {
            continue;
        }
        let raw: RawEvent = serde_json::from_str(&event.data)
            .map_err(|e| format!("could not parse Anthropic SSE event {}: {e}", event.event))?;

        match raw {
            RawEvent::MessageStart { message } => {
                saw_message_start = true;
                output.response_id = Some(message.id);
                if let Some(usage) = message.usage {
                    apply_usage(output, &usage);
                }
            }
            RawEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                handle_block_start(output, tx, index, content_block, &mut block_positions);
            }
            RawEvent::ContentBlockDelta { index, delta } => {
                handle_block_delta(output, tx, index, delta, &mut block_positions);
            }
            RawEvent::ContentBlockStop { index } => {
                handle_block_stop(output, tx, index, &mut block_positions);
            }
            RawEvent::MessageDelta { delta, usage } => {
                if let Some(stop_reason) = delta.stop_reason {
                    output.stop_reason = map_stop_reason(&stop_reason);
                }
                if let Some(usage) = usage {
                    apply_usage(output, &usage);
                }
            }
            RawEvent::MessageStop => saw_message_stop = true,
            RawEvent::Error { error } => return Err(format!("{}: {}", error.kind, error.message)),
            RawEvent::Ping | RawEvent::Other => {}
        }
    }

    if saw_message_start && !saw_message_stop {
        return Err("Anthropic stream ended before message_stop".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// 工具调用 id 规整：Anthropic 要求 `^[a-zA-Z0-9_-]+$` 且不超过 64 字符。
fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

/// 流式累积中单个内容块的状态。
#[derive(Debug)]
struct BlockState {
    /// 线上协议中的块序号
    raw_index: usize,
    /// 在 `output.content` 中的位置
    content_position: usize,
    /// 工具调用的 partial JSON 累积缓冲
    partial_json: Option<String>,
}

fn handle_block_start(
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
    raw_index: usize,
    block: RawContentBlock,
    positions: &mut Vec<BlockState>,
) {
    let content_position = output.content.len();
    let mut partial_json = None;
    let event = match block {
        RawContentBlock::Text { .. } => {
            output.content.push(AssistantContent::Text(TextContent {
                text: String::new(),
                text_signature: None,
            }));
            AssistantEvent::TextStart {
                index: content_position,
            }
        }
        RawContentBlock::Thinking { .. } => {
            output
                .content
                .push(AssistantContent::Thinking(ThinkingContent {
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: false,
                }));
            AssistantEvent::ThinkingStart {
                index: content_position,
            }
        }
        RawContentBlock::RedactedThinking { data } => {
            output
                .content
                .push(AssistantContent::Thinking(ThinkingContent {
                    thinking: "[Reasoning redacted]".to_string(),
                    thinking_signature: Some(data),
                    redacted: true,
                }));
            AssistantEvent::ThinkingStart {
                index: content_position,
            }
        }
        RawContentBlock::ToolUse { id, name, .. } => {
            partial_json = Some(String::new());
            output.content.push(AssistantContent::ToolCall(ToolCall {
                id: normalize_tool_call_id(&id),
                name,
                arguments: serde_json::Value::Object(serde_json::Map::new()),
                thought_signature: None,
            }));
            AssistantEvent::ToolCallStart {
                index: content_position,
            }
        }
        RawContentBlock::Other => return,
    };
    positions.push(BlockState {
        raw_index,
        content_position,
        partial_json,
    });
    let _ = tx.send(event);
}

fn handle_block_delta(
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
    raw_index: usize,
    delta: RawDelta,
    positions: &mut [BlockState],
) {
    let Some(slot) = positions.iter_mut().find(|s| s.raw_index == raw_index) else {
        return;
    };
    let index = slot.content_position;
    match (delta, &mut output.content[index]) {
        (RawDelta::TextDelta { text }, AssistantContent::Text(block)) => {
            block.text.push_str(&text);
            let _ = tx.send(AssistantEvent::TextDelta { index, delta: text });
        }
        (RawDelta::ThinkingDelta { thinking }, AssistantContent::Thinking(block)) => {
            block.thinking.push_str(&thinking);
            let _ = tx.send(AssistantEvent::ThinkingDelta {
                index,
                delta: thinking,
            });
        }
        (RawDelta::InputJsonDelta { partial_json }, AssistantContent::ToolCall(_)) => {
            if let Some(buffer) = &mut slot.partial_json {
                buffer.push_str(&partial_json);
            }
            let _ = tx.send(AssistantEvent::ToolCallDelta {
                index,
                delta: partial_json,
            });
        }
        (RawDelta::SignatureDelta { signature }, AssistantContent::Thinking(block)) => {
            block
                .thinking_signature
                .get_or_insert_with(String::new)
                .push_str(&signature);
        }
        _ => {}
    }
}

fn handle_block_stop(
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
    raw_index: usize,
    positions: &mut Vec<BlockState>,
) {
    let Some(slot_position) = positions.iter().position(|s| s.raw_index == raw_index) else {
        return;
    };
    let slot = positions.remove(slot_position);
    let index = slot.content_position;
    let event = match &mut output.content[index] {
        AssistantContent::Text(_) => AssistantEvent::TextEnd { index },
        AssistantContent::Thinking(_) => AssistantEvent::ThinkingEnd { index },
        AssistantContent::ToolCall(call) => {
            if let Some(buffer) = slot.partial_json {
                call.arguments = parse_streaming_json(&buffer);
            }
            AssistantEvent::ToolCallEnd {
                index,
                tool_call: call.clone(),
            }
        }
    };
    let _ = tx.send(event);
}

/// 解析流式累积的 partial JSON；被截断时做最大努力修复（未闭合的括号/引号）。
fn parse_streaming_json(partial: &str) -> serde_json::Value {
    if let Ok(value) = serde_json::from_str(partial) {
        return value;
    }
    // 截断修复：补齐未闭合的字符串与括号
    let mut fixed = partial.trim_end().to_string();
    let mut in_string = false;
    let mut escaped = false;
    let mut stack = Vec::new();
    for c in fixed.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' | '[' if !in_string => stack.push(c),
            '}' | ']' if !in_string => {
                stack.pop();
            }
            _ => {}
        }
    }
    if in_string {
        fixed.push('"');
    }
    // 去掉可能被截断的尾键值
    while let Some(c) = fixed.chars().last() {
        if c == '{' || c == '[' || c == '"' || c == '}' || c == ']' {
            break;
        }
        fixed.pop();
    }
    if fixed.trim_end().ends_with(':') {
        fixed.push_str("null");
    }
    while let Some(open) = stack.pop() {
        fixed.push(if open == '{' { '}' } else { ']' });
    }
    serde_json::from_str(&fixed)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
}

const fn apply_usage(output: &mut AssistantMessage, usage: &RawUsage) {
    // 只更新出现的字段：message_delta 时代理可能省略 input_tokens，
    // 需保留 message_start 中已记录的值
    if let Some(input) = usage.input_tokens {
        output.usage.input = input;
    }
    if let Some(output_tokens) = usage.output_tokens {
        output.usage.output = output_tokens;
    }
    if let Some(cache_read) = usage.cache_read_input_tokens {
        output.usage.cache_read = cache_read;
    }
    if let Some(cache_write) = usage.cache_creation_input_tokens {
        output.usage.cache_write = cache_write;
    }
    output.usage.total_tokens = output.usage.input
        + output.usage.output
        + output.usage.cache_read
        + output.usage.cache_write;
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "max_tokens" | "model_context_window_exceeded" => StopReason::Length,
        "tool_use" | "pause_turn" => StopReason::ToolUse,
        "refusal" | "sensitive" => StopReason::Error,
        // "end_turn" / "stop_sequence" / 未知值均按正常结束处理
        _ => StopReason::Stop,
    }
}

// ── 线上协议的反序列化类型 ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawEvent {
    MessageStart {
        message: RawMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: RawContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: RawDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: RawMessageDelta,
        usage: Option<RawUsage>,
    },
    MessageStop,
    Error {
        error: RawError,
    },
    Ping,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawMessageStart {
    id: String,
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawContentBlock {
    Text {
        #[serde(rename = "text")]
        _text: String,
    },
    Thinking {
        #[serde(rename = "thinking")]
        _thinking: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(rename = "input")]
        _input: Option<serde_json::Value>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawDelta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    SignatureDelta {
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawMessageDelta {
    stop_reason: Option<String>,
}

/// Anthropic 线上 usage 结构（字段名由线上协议决定）。
#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)]
struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}
