//! OpenAI 流式事件处理：文本/思考/工具内容块状态机与 usage 映射。

use std::collections::HashMap;

use super::raw::{RawDelta, RawUsage};
use crate::AssistantEvent;
use crate::providers::shared::parse_streaming_json;
use crate::types::{
    AssistantContent, AssistantMessage, StopReason, TextContent, ThinkingContent, ToolCall,
};

/// 工具调用流式累积状态。
#[derive(Debug)]
pub(super) struct ToolBlockState {
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

/// 非空字段才参与回退链：返回 `(回放字段名, 文本)`。
fn first_non_empty(field: &'static str, value: Option<String>) -> Option<(&'static str, String)> {
    value.filter(|s| !s.is_empty()).map(|s| (field, s))
}

pub(super) fn handle_delta(
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
    let reasoning = first_non_empty("reasoning_content", delta.reasoning_content)
        .or_else(|| first_non_empty("reasoning", delta.reasoning))
        .or_else(|| first_non_empty("reasoning_text", delta.reasoning_text));
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

pub(super) fn finish_tool_block(
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

pub(super) fn apply_usage(output: &mut AssistantMessage, usage: &RawUsage) {
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

pub(super) fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
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
