//! OpenAI Chat Completions API provider（含 DeepSeek 等兼容端点）。
//!
//! 消息与流式映射借鉴 pi-ai 的 `openai-completions.ts`：
//! - assistant 文本合并为纯字符串 content（数组形式会让部分模型镜像 block 结构）
//! - thinking 块以首个块的 signature 字段名回放（默认 `reasoning_content`）
//! - 工具调用按流 index 累积 partial JSON，结束后最大努力解析
//! - usage 从 `stream_options.include_usage` 的最终 chunk 读取
//!
//! M1 未实现：Responses API、grammar tools、deferred tools（见 ADR-0001）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::providers::retry::{RequestError, RetryPolicy, sleep_or_cancel};
use crate::stream::{AssistantStream, Provider, StreamOptions, channel};
use crate::types::{
    AssistantContent, AssistantMessage, Context, Message, Model, StopReason, TextContent,
    ThinkingContent, ThinkingLevel, ToolCall, ToolResultMessage, UserContent,
};
use crate::{AssistantEvent, now_millis};

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
        let (tx, stream) = channel();
        let mut output = empty_output(model);
        let client = self.client.clone();
        let call = StreamCall {
            base_url: model.base_url.clone(),
            api_key: self.resolve_api_key(options),
            request: build_request(model, context, options, &self.compat),
            timeout_ms: options.timeout_ms,
        };
        let model_for_cost = model.clone();
        let retry_policy = self.retry_policy;
        let span = tracing::info_span!(
            "llm_request",
            provider = %model.provider,
            model = %model.id,
            base_url = %model.base_url,
        );

        tokio::spawn(
            async move {
                let started = Instant::now();
                let mut retries = 0u32;
                let result = loop {
                    let attempt = run(&client, &call, cancel.clone(), &mut output, &tx).await;
                    let Err(error) = attempt else {
                        break Ok(());
                    };
                    // 只重试流建立前的瞬时错误（见 retry 模块文档）；
                    // 取消与致命错误直接终止
                    if !error.retryable
                        || retries >= retry_policy.max_retries
                        || cancel.is_cancelled()
                    {
                        break Err(error.message);
                    }
                    retries += 1;
                    let delay = retry_policy.delay(retries);
                    tracing::warn!(
                        error = %error.message,
                        retry = retries,
                        max_retries = retry_policy.max_retries,
                        delay_ms = delay.as_millis(),
                        "llm request failed, retrying"
                    );
                    if sleep_or_cancel(delay, &cancel).await {
                        break Err("request aborted".to_string());
                    }
                    // 防御性重置：重试边界保证失败时未发出任何事件，
                    // output 应未被触碰；重置使该不变式显式成立
                    output = empty_output(&model_for_cost);
                };
                let elapsed_ms = started.elapsed().as_millis();
                match result {
                    Ok(()) => {
                        model_for_cost.calculate_cost(&mut output.usage);
                        tracing::debug!(
                            stop_reason = ?output.stop_reason,
                            input_tokens = output.usage.input,
                            output_tokens = output.usage.output,
                            cache_read_tokens = output.usage.cache_read,
                            elapsed_ms,
                            "llm request finished"
                        );
                        let _ = tx.send(AssistantEvent::Done {
                            message: Box::new(output),
                        });
                    }
                    Err(error) => {
                        model_for_cost.calculate_cost(&mut output.usage);
                        if cancel.is_cancelled() {
                            tracing::debug!(elapsed_ms, "llm request aborted");
                        } else {
                            tracing::warn!(%error, elapsed_ms, "llm request failed");
                        }
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
            }
            .instrument(span),
        );

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
        result = builder.json(&call.request).send() => result.map_err(|e| RequestError::from_reqwest(&e))?,
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
        return Err(output
            .error_message
            .clone()
            .unwrap_or_else(|| "provider returned an error stop reason".to_string()));
    }
    if !has_finish_reason {
        return Err("stream ended without finish_reason".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// 工具调用流式累积状态。
#[derive(Debug)]
struct ToolBlockState {
    /// 在 `output.content` 中的位置
    content_position: usize,
    /// partial JSON 累积缓冲
    partial_json: String,
}

fn ensure_text_block(
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
    text_block: &mut Option<usize>,
) -> usize {
    if let Some(index) = *text_block {
        return index;
    }
    let index = output.content.len();
    output.content.push(AssistantContent::Text(TextContent {
        text: String::new(),
        text_signature: None,
    }));
    *text_block = Some(index);
    let _ = tx.send(AssistantEvent::TextStart { index });
    index
}

fn ensure_thinking_block(
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
    thinking_block: &mut Option<usize>,
    signature: &str,
) -> usize {
    if let Some(index) = *thinking_block {
        return index;
    }
    let index = output.content.len();
    output
        .content
        .push(AssistantContent::Thinking(ThinkingContent {
            thinking: String::new(),
            thinking_signature: Some(signature.to_string()),
            redacted: false,
        }));
    *thinking_block = Some(index);
    let _ = tx.send(AssistantEvent::ThinkingStart { index });
    index
}

fn handle_delta(
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
    delta: RawDelta,
    text_block: &mut Option<usize>,
    thinking_block: &mut Option<usize>,
    tool_blocks: &mut HashMap<usize, ToolBlockState>,
) {
    if let Some(content) = delta.content
        && !content.is_empty()
    {
        let index = ensure_text_block(output, tx, text_block);
        if let AssistantContent::Text(block) = &mut output.content[index] {
            block.text.push_str(&content);
        }
        let _ = tx.send(AssistantEvent::TextDelta {
            index,
            delta: content,
        });
    }

    // reasoning_content / reasoning / reasoning_text 取首个非空字段（避免重复）
    let reasoning = delta
        .reasoning_content
        .filter(|s| !s.is_empty())
        .map(|s| ("reasoning_content", s))
        .or_else(|| {
            delta
                .reasoning
                .filter(|s| !s.is_empty())
                .map(|s| ("reasoning", s))
        })
        .or_else(|| {
            delta
                .reasoning_text
                .filter(|s| !s.is_empty())
                .map(|s| ("reasoning_text", s))
        });
    if let Some((field, text)) = reasoning {
        let index = ensure_thinking_block(output, tx, thinking_block, field);
        if let AssistantContent::Thinking(block) = &mut output.content[index] {
            block.thinking.push_str(&text);
        }
        let _ = tx.send(AssistantEvent::ThinkingDelta { index, delta: text });
    }

    for tool_call in delta.tool_calls.unwrap_or_default() {
        let stream_index = tool_call.index.unwrap_or(0);
        let state = tool_blocks.entry(stream_index).or_insert_with(|| {
            let content_position = output.content.len();
            output.content.push(AssistantContent::ToolCall(ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: serde_json::Value::Object(serde_json::Map::new()),
                thought_signature: None,
            }));
            let _ = tx.send(AssistantEvent::ToolCallStart {
                index: content_position,
            });
            ToolBlockState {
                content_position,
                partial_json: String::new(),
            }
        });
        let index = state.content_position;
        let AssistantContent::ToolCall(call) = &mut output.content[index] else {
            continue;
        };
        if let Some(id) = tool_call.id
            && call.id.is_empty()
        {
            call.id = id;
        }
        if let Some(function) = tool_call.function {
            if let Some(name) = function.name
                && call.name.is_empty()
            {
                call.name = name;
            }
            if let Some(arguments) = function.arguments {
                state.partial_json.push_str(&arguments);
                let _ = tx.send(AssistantEvent::ToolCallDelta {
                    index,
                    delta: arguments,
                });
            }
        }
    }
}

fn finish_tool_block(
    output: &mut AssistantMessage,
    tx: &tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
    state: &ToolBlockState,
) {
    let index = state.content_position;
    if let AssistantContent::ToolCall(call) = &mut output.content[index] {
        call.arguments = parse_streaming_json(&state.partial_json);
        let _ = tx.send(AssistantEvent::ToolCallEnd {
            index,
            tool_call: call.clone(),
        });
    }
}

/// 与 Anthropic provider 相同的截断 JSON 最大努力修复。
fn parse_streaming_json(partial: &str) -> serde_json::Value {
    if let Ok(value) = serde_json::from_str(partial) {
        return value;
    }
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

fn apply_usage(output: &mut AssistantMessage, usage: &RawUsage) {
    output.usage.input = usage.prompt_tokens.unwrap_or(0);
    output.usage.output = usage.completion_tokens.unwrap_or(0);
    output.usage.cache_read = usage
        .prompt_tokens_details
        .as_ref()
        .and_then(|d| d.cached_tokens)
        .unwrap_or(0);
    output.usage.reasoning = usage
        .completion_tokens_details
        .as_ref()
        .and_then(|d| d.reasoning_tokens);
    output.usage.total_tokens = usage
        .total_tokens
        .unwrap_or(output.usage.input + output.usage.output);
}

fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "tool_calls" | "function_call" => (StopReason::ToolUse, None),
        other => (
            StopReason::Error,
            Some(format!("provider finish_reason: {other}")),
        ),
    }
}

// ── 线上协议的反序列化类型 ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawChunk {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<RawChoice>,
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawChoice {
    delta: Option<RawDelta>,
    finish_reason: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    reasoning_text: Option<String>,
    tool_calls: Option<Vec<RawToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct RawToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    function: Option<RawFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct RawFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    prompt_tokens_details: Option<RawPromptTokensDetails>,
    completion_tokens_details: Option<RawCompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct RawPromptTokensDetails {
    cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawCompletionTokensDetails {
    reasoning_tokens: Option<u64>,
}
