//! Anthropic Messages API provider。
//!
//! 流式事件映射忠实复刻 pi-ai 的 `anthropic-messages.ts`：
//! `message_start`（初始 usage）→ 内容块 `start/delta/stop` →
//! `message_delta`（`stop_reason` + 最终 usage）→ `Done`。
//! M1 未实现：prompt caching、OAuth 身份、deferred tools（见 ADR-0001）。

mod events;
mod raw;

use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::AssistantEvent;
use crate::providers::retry::{RequestError, RetryPolicy};
use crate::providers::shared::{StreamAttempt, spawn_stream};
use crate::stream::{AssistantStream, Provider, StreamOptions};
use crate::types::{
    AssistantContent, AssistantMessage, Context, Message, Model, ThinkingLevel, UserContent,
};
// 供 `tests.rs` 经 `use super::*` 使用（与拆分前的 import 语义一致）
#[allow(unused_imports)]
use crate::types::{StopReason, TextContent, ToolCall};

use events::{
    BlockState, apply_usage, handle_block_delta, handle_block_start, handle_block_stop,
    map_stop_reason,
};
use raw::RawEvent;

/// Anthropic Messages API provider。
pub struct AnthropicProvider {
    client: reqwest::Client,
    /// 缺省 API key（`StreamOptions.api_key` 优先）
    api_key: Option<String>,
    /// 失败重试策略
    retry_policy: RetryPolicy,
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
        Self {
            client,
            api_key,
            retry_policy: RetryPolicy::default(),
        }
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
        let attempt = AnthropicAttempt {
            client: self.client.clone(),
            base_url: model.base_url.clone(),
            api_key: self.resolve_api_key(options),
            request: build_request(model, context, options),
            timeout_ms: options.timeout_ms,
        };
        spawn_stream(model, self.retry_policy, cancel, attempt)
    }
}

/// Anthropic 的一次流式请求尝试（[`spawn_stream`] 的 provider 侧）。
struct AnthropicAttempt {
    client: reqwest::Client,
    base_url: String,
    /// 分层的 api_key（Err 在尝试时转为致命错误，保持原解析时序）
    api_key: Result<String, String>,
    request: serde_json::Value,
    timeout_ms: Option<u64>,
}

impl StreamAttempt for AnthropicAttempt {
    async fn run(
        &mut self,
        output: &mut AssistantMessage,
        tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
        cancel: CancellationToken,
    ) -> Result<(), RequestError> {
        run(
            &self.client,
            &self.base_url,
            &self.api_key,
            &self.request,
            self.timeout_ms,
            cancel,
            output,
            tx,
        )
        .await
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
    api_key: &Result<String, String>,
    request: &serde_json::Value,
    timeout_ms: Option<u64>,
    cancel: CancellationToken,
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
) -> Result<(), RequestError> {
    let api_key = api_key
        .as_ref()
        .map_err(|e| RequestError::fatal(e.clone()))?;
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
        () = cancel.cancelled() => return Err(RequestError::fatal("request aborted".to_string())),
        result = builder.json(request).send() => result.map_err(|e| RequestError::from_reqwest(&e))?,
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(RequestError::from_status(status, &body));
    }

    let _ = tx.send(AssistantEvent::Start);
    // Start 已发出，流中错误不得重试（会产生重复事件）
    process_events(response.bytes_stream().eventsource(), cancel, output, tx)
        .await
        .map_err(RequestError::fatal)
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
