//! Anthropic 流式事件处理：内容块状态机、usage 与 stop_reason 映射。

use super::raw::{RawContentBlock, RawDelta, RawUsage};
use crate::AssistantEvent;
use crate::types::{
    AssistantContent, AssistantMessage, StopReason, TextContent, ThinkingContent, ToolCall,
};

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
pub(super) struct BlockState {
    /// 线上协议中的块序号
    raw_index: usize,
    /// 在 `output.content` 中的位置
    content_position: usize,
    /// 工具调用的 partial JSON 累积缓冲
    partial_json: Option<String>,
}

pub(super) fn handle_block_start(
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

pub(super) fn handle_block_delta(
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

pub(super) fn handle_block_stop(
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

pub(super) const fn apply_usage(output: &mut AssistantMessage, usage: &RawUsage) {
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

pub(super) fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "max_tokens" | "model_context_window_exceeded" => StopReason::Length,
        "tool_use" | "pause_turn" => StopReason::ToolUse,
        "refusal" | "sensitive" => StopReason::Error,
        // "end_turn" / "stop_sequence" / 未知值均按正常结束处理
        _ => StopReason::Stop,
    }
}
