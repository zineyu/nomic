//! 两个 provider 共用的内部工具：空输出构造、截断 JSON 修复。

use crate::now_millis;
use crate::types::{AssistantMessage, Model, StopReason};

/// 空的 assistant 输出消息：流式累积的起点（重试边界的防御性重置也用它）。
pub(super) fn empty_output(model: &Model) -> AssistantMessage {
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

/// 解析流式累积的 partial JSON；被截断时做最大努力修复（未闭合的括号/引号）。
pub(super) fn parse_streaming_json(partial: &str) -> serde_json::Value {
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
