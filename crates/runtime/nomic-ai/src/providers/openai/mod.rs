//! OpenAI Chat Completions API provider（含 DeepSeek 等兼容端点）。
//!
//! 消息与流式映射借鉴 pi-ai 的 `openai-completions.ts`：
//! - assistant 文本合并为纯字符串 content（数组形式会让部分模型镜像 block 结构）
//! - thinking 块以首个块的 signature 字段名回放（默认 `reasoning_content`）
//! - 工具调用按流 index 累积 partial JSON，结束后最大努力解析
//! - usage 从 `stream_options.include_usage` 的最终 chunk 读取
//!
//! M1 未实现：Responses API、grammar tools、deferred tools（见 ADR-0001）。

mod raw;
mod tool_state;

use std::collections::HashMap;
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::AssistantEvent;
use crate::providers::retry::{RequestError, RetryPolicy};
use crate::providers::shared::{StreamAttempt, spawn_stream};
use crate::stream::{AssistantStream, Provider, StreamOptions};
use crate::types::{
    AssistantContent, AssistantMessage, Context, Message, Model, StopReason, ThinkingContent,
    ThinkingLevel, ToolCall, ToolResultMessage, UserContent,
};
// 供 `tests.rs` 经 `use super::*` 使用（与拆分前的 import 语义一致）
#[allow(unused_imports)]
use crate::types::TextContent;

use raw::RawChunk;
use tool_state::{ToolBlockState, apply_usage, finish_tool_block, handle_delta, map_stop_reason};

/// `max_tokens` 请求字段的命名差异。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaxTokensField {
    /// `max_completion_tokens`（OpenAI 新模型）
    MaxCompletionTokens,
    /// `max_tokens`（多数兼容端点）
    #[default]
    MaxTokens,
}

/// OpenAI 兼容端点的差异修补。
///
/// 只建模真实会碰到的差异；pi 中 20+ 字段的全集是按需长出来的，这里同样按需扩展。
#[derive(Debug, Clone, Default)]
pub struct OpenAiCompat {
    /// `max_tokens` 字段命名
    pub max_tokens_field: MaxTokensField,
    /// 是否支持 `stream_options.include_usage`（默认 true）
    pub supports_usage_in_streaming: Option<bool>,
    /// 推理模型的系统消息是否用 `developer` 角色（默认 false，用 `system`）
    pub supports_developer_role: Option<bool>,
    /// 工具结果是否必须带 `name` 字段（默认 false）
    pub requires_tool_result_name: bool,
    /// 工具结果后是否不允许直接跟 user 消息（默认 false）
    pub requires_assistant_after_tool_result: bool,
}

/// OpenAI Completions 兼容 provider。
pub struct OpenAiProvider {
    client: reqwest::Client,
    /// 缺省 API key（`StreamOptions.api_key` 优先）
    api_key: Option<String>,
    /// 兼容端点修补
    compat: OpenAiCompat,
    /// 失败重试策略
    retry_policy: RetryPolicy,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider").finish_non_exhaustive()
    }
}

impl OpenAiProvider {
    /// 创建 provider；`api_key` 为 `None` 时每次请求回退到 `OPENAI_API_KEY` 环境变量。
    pub fn new(api_key: Option<String>, compat: OpenAiCompat) -> Self {
        tracing::debug!(
            has_api_key = api_key.is_some(),
            max_tokens_field = ?compat.max_tokens_field,
            supports_usage_in_streaming = ?compat.supports_usage_in_streaming,
            "OpenAiProvider created"
        );
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");
        Self {
            client,
            api_key,
            compat,
            retry_policy: RetryPolicy::default(),
        }
    }

    fn resolve_api_key(&self, options: &StreamOptions) -> String {
        if let Some(key) = &options.api_key {
            return key.clone();
        }
        if let Some(key) = &self.api_key {
            return key.clone();
        }
        // 本地/代理端点常见约定：无 key 也放行，由端点自行决定
        std::env::var("OPENAI_API_KEY").unwrap_or_default()
    }
}

impl Provider for OpenAiProvider {
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
        cancel: CancellationToken,
    ) -> AssistantStream {
        tracing::debug!(
            model = %model.id,
            base_url = %model.base_url,
            messages = context.messages.len(),
            tools = context.tools.len(),
            has_system_prompt = context.system_prompt.is_some(),
            reasoning = ?options.reasoning,
            "OpenAI stream request"
        );
        let attempt = OpenAiAttempt {
            client: self.client.clone(),
            call: StreamCall {
                base_url: model.base_url.clone(),
                api_key: self.resolve_api_key(options),
                request: build_request(model, context, options, &self.compat),
                timeout_ms: options.timeout_ms,
            },
        };
        spawn_stream(model, self.retry_policy, cancel, attempt)
    }
}

/// OpenAI 的一次流式请求尝试（[`spawn_stream`] 的 provider 侧）。
struct OpenAiAttempt {
    client: reqwest::Client,
    call: StreamCall,
}

impl StreamAttempt for OpenAiAttempt {
    async fn run(
        &mut self,
        output: &mut AssistantMessage,
        tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
        cancel: CancellationToken,
    ) -> Result<(), RequestError> {
        run(&self.client, &self.call, cancel, output, tx).await
    }
}

// ── 请求构造 ─────────────────────────────────────────────────────────────────

const fn reasoning_effort(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => "high",
    }
}

fn build_request(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    compat: &OpenAiCompat,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model.id,
        "messages": convert_messages(model, context, compat),
        "stream": true,
    });

    let max_tokens = options.max_tokens.unwrap_or(model.max_tokens);
    if max_tokens > 0 {
        match compat.max_tokens_field {
            MaxTokensField::MaxCompletionTokens => {
                body["max_completion_tokens"] = serde_json::json!(max_tokens);
            }
            MaxTokensField::MaxTokens => body["max_tokens"] = serde_json::json!(max_tokens),
        }
    }
    if compat.supports_usage_in_streaming.unwrap_or(true) {
        body["stream_options"] = serde_json::json!({ "include_usage": true });
    }
    if let Some(temperature) = options.temperature
        && !model.reasoning
    {
        body["temperature"] = serde_json::json!(temperature);
    }
    if model.reasoning
        && let Some(level) = options.reasoning
    {
        body["reasoning_effort"] = serde_json::json!(reasoning_effort(level));
    }
    if !context.tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            context
                .tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        },
                    })
                })
                .collect(),
        );
    }
    body
}

fn convert_messages(
    model: &Model,
    context: &Context,
    compat: &OpenAiCompat,
) -> Vec<serde_json::Value> {
    let mut params: Vec<serde_json::Value> = Vec::new();

    if let Some(system) = &context.system_prompt {
        let use_developer = model.reasoning && compat.supports_developer_role.unwrap_or(false);
        let role = if use_developer { "developer" } else { "system" };
        params.push(serde_json::json!({ "role": role, "content": system }));
    }

    let mut last_was_tool_result = false;
    let mut iter = context.messages.iter().peekable();
    while let Some(message) = iter.next() {
        // 部分端点不允许 user 直接跟在 tool 结果之后，插入桥接 assistant 消息
        if compat.requires_assistant_after_tool_result
            && last_was_tool_result
            && matches!(message, Message::User(_))
        {
            params.push(serde_json::json!({ "role": "assistant", "content": "I have processed the tool results." }));
        }
        last_was_tool_result = matches!(message, Message::ToolResult(_));

        match message {
            Message::User(user) => match &user.content {
                crate::types::UserMessageContent::Text(text) => {
                    params.push(serde_json::json!({ "role": "user", "content": text }));
                }
                crate::types::UserMessageContent::Blocks(blocks) => {
                    let parts: Vec<serde_json::Value> = blocks
                        .iter()
                        .map(|block| match block {
                            UserContent::Text(text) => {
                                serde_json::json!({ "type": "text", "text": text.text })
                            }
                            UserContent::Image(image) => serde_json::json!({
                                "type": "image_url",
                                "image_url": { "url": format!("data:{};base64,{}", image.mime_type, image.data) },
                            }),
                        })
                        .collect();
                    if parts.is_empty() {
                        continue;
                    }
                    params.push(serde_json::json!({ "role": "user", "content": parts }));
                }
            },
            Message::Assistant(assistant) => {
                if let Some(msg) = convert_assistant_message(assistant) {
                    params.push(msg);
                }
            }
            Message::ToolResult(_) => {
                // OpenAI 协议：每个工具结果是一条独立的 role=tool 消息（不合并）
                let mut current = Some(message);
                while let Some(Message::ToolResult(result)) = current {
                    params.push(convert_tool_result(result, compat));
                    current = iter.next_if(|m| matches!(m, Message::ToolResult(_)));
                }
            }
        }
    }
    params
}

/// 转换一条 assistant 消息；空消息（无内容且无工具调用）返回 `None`。
fn convert_assistant_message(assistant: &AssistantMessage) -> Option<serde_json::Value> {
    let mut msg = serde_json::json!({ "role": "assistant", "content": serde_json::Value::Null });

    let text: String = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::Text(t) if !t.text.trim().is_empty() => Some(t.text.as_str()),
            _ => None,
        })
        .collect();
    if !text.is_empty() {
        // 始终以纯字符串发送 assistant content（API 标准形式）
        msg["content"] = serde_json::json!(text);
    }

    let thinking: Vec<&ThinkingContent> = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::Thinking(t) if !t.thinking.trim().is_empty() => Some(t),
            _ => None,
        })
        .collect();
    if let Some(first) = thinking.first() {
        // 用首个 thinking 块的 signature 作为回放字段名（llama.cpp 等为 "reasoning"）
        let field = first
            .thinking_signature
            .as_deref()
            .unwrap_or("reasoning_content");
        let joined = thinking
            .iter()
            .map(|t| t.thinking.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        msg[field] = serde_json::json!(joined);
    }

    let tool_calls: Vec<&ToolCall> = assistant
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();
    if !tool_calls.is_empty() {
        msg["tool_calls"] = serde_json::Value::Array(
            tool_calls
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "id": normalize_tool_call_id(&call.id),
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        },
                    })
                })
                .collect(),
        );
    }

    // 跳过既无内容又无工具调用的空 assistant 消息（如被中止的响应），
    // 部分端点要求 "content 与 tool_calls 至少其一"
    if msg["content"].is_null() && msg.get("tool_calls").is_none() {
        return None;
    }
    Some(msg)
}

/// 转换一条工具结果消息（`role=tool`）。
fn convert_tool_result(result: &ToolResultMessage, compat: &OpenAiCompat) -> serde_json::Value {
    let text: String = result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContent::Text(t) => Some(t.text.as_str()),
            UserContent::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let has_images = result
        .content
        .iter()
        .any(|b| matches!(b, UserContent::Image(_)));
    let result_text = if !text.is_empty() {
        text
    } else if has_images {
        "(see attached image)".to_string()
    } else {
        "(no tool output)".to_string()
    };
    let mut msg = serde_json::json!({
        "role": "tool",
        "content": result_text,
        "tool_call_id": normalize_tool_call_id(&result.tool_call_id),
    });
    if compat.requires_tool_result_name {
        msg["name"] = serde_json::json!(result.tool_name);
    }
    msg
}

/// OpenAI 限制工具调用 id 不超过 40 字符。
fn normalize_tool_call_id(id: &str) -> String {
    id.chars().take(40).collect()
}

// ── 流式处理 ─────────────────────────────────────────────────────────────────

/// 一次流式请求的全部输入。
#[derive(Debug)]
struct StreamCall {
    base_url: String,
    api_key: String,
    request: serde_json::Value,
    timeout_ms: Option<u64>,
}

async fn run(
    client: &reqwest::Client,
    call: &StreamCall,
    cancel: CancellationToken,
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
) -> Result<(), RequestError> {
    let mut builder = client
        .post(format!("{}/chat/completions", call.base_url))
        .header("content-type", "application/json");
    if !call.api_key.is_empty() {
        builder = builder.bearer_auth(&call.api_key);
    }
    if let Some(ms) = call.timeout_ms {
        builder = builder.timeout(Duration::from_millis(ms));
    }

    let response = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(RequestError::fatal("request aborted".to_string())),
        result = builder.json(&call.request).send() => result.map_err(|e| {
            tracing::debug!(error = %e, "OpenAI HTTP request failed");
            RequestError::from_reqwest(&e)
        })?,
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        tracing::warn!(status = %status, body_len = body.len(), "OpenAI API error");
        return Err(RequestError::from_status(status, &body));
    }

    tracing::debug!(status = %status, "OpenAI SSE stream established");
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
    // 流式状态：文本/思考块各自惰性创建（先出现者在前）；工具调用按流 index 累积
    let mut text_block: Option<usize> = None;
    let mut thinking_block: Option<usize> = None;
    let mut tool_blocks: HashMap<usize, ToolBlockState> = HashMap::new();
    let mut has_finish_reason = false;

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
        if event.data == "[DONE]" {
            break;
        }
        let chunk: RawChunk = serde_json::from_str(&event.data)
            .map_err(|e| format!("could not parse OpenAI SSE chunk: {e}"))?;

        if output.response_id.is_none() {
            output.response_id.clone_from(&chunk.id);
        }
        if let Some(response_model) = &chunk.model
            && !response_model.is_empty()
            && *response_model != output.model
        {
            output.response_model = Some(response_model.clone());
        }
        if let Some(usage) = &chunk.usage {
            apply_usage(output, usage);
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            continue;
        };
        // 部分端点（如 Moonshot）把 usage 放在 choice 上
        if chunk.usage.is_none()
            && let Some(usage) = &choice.usage
        {
            apply_usage(output, usage);
        }

        if let Some(finish_reason) = &choice.finish_reason {
            let (stop_reason, error_message) = map_stop_reason(finish_reason);
            output.stop_reason = stop_reason;
            if let Some(message) = error_message {
                output.error_message = Some(message);
            }
            has_finish_reason = true;
        }

        if let Some(delta) = choice.delta {
            handle_delta(
                output,
                tx,
                delta,
                &mut text_block,
                &mut thinking_block,
                &mut tool_blocks,
            );
        }
    }

    // 收尾所有未关闭的工具调用块
    let mut tool_states: Vec<(usize, ToolBlockState)> = tool_blocks.into_iter().collect();
    tool_states.sort_by_key(|(stream_index, _)| *stream_index);
    for (_, state) in tool_states {
        finish_tool_block(output, tx, &state);
    }

    if output.stop_reason == StopReason::Error {
        let msg = output
            .error_message
            .clone()
            .unwrap_or_else(|| "provider returned an error stop reason".to_string());
        tracing::warn!(error = %msg, "OpenAI stream ended with error stop reason");
        return Err(msg);
    }
    if !has_finish_reason {
        tracing::warn!("OpenAI stream ended without finish_reason");
        return Err("stream ended without finish_reason".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
