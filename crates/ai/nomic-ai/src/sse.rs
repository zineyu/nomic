//! 剧本到 OpenAI Chat SSE 帧的编码。

use rig_core::http_client::StatusCode;
use rig_core::test_utils::MockHttpResponse;
use serde_json::json;

/// 剧本模型名。
pub const SCRIPTED_MODEL: &str = "scripted-model";

/// 剧本事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptedEvent {
    /// 文本增量。
    Token(String),
    /// 工具调用请求：wire 上的调用 id、工具名与参数 JSON。
    ToolCall {
        /// wire 调用 id。
        id: String,
        /// 工具名。
        tool: String,
        /// 参数 JSON。
        arguments: serde_json::Value,
    },
}

impl ScriptedEvent {
    /// 文本增量。
    pub fn token(text: impl Into<String>) -> Self {
        Self::Token(text.into())
    }

    /// 工具调用请求。
    pub fn tool_call(
        id: impl Into<String>,
        tool: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self::ToolCall {
            id: id.into(),
            tool: tool.into(),
            arguments,
        }
    }
}

/// 一次 respond 的剧本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptedTurn {
    /// 产出给定事件序列后正常结束。
    Reply(Vec<ScriptedEvent>),
    /// 以 HTTP 传输失败结束（无 provider 应答）。
    TransportFailure(String),
}

impl ScriptedTurn {
    /// 纯文本回复。
    pub fn reply(tokens: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Reply(tokens.into_iter().map(ScriptedEvent::token).collect())
    }
}

/// 一个回合的 HTTP 响应。
pub fn response(turn: &ScriptedTurn) -> MockHttpResponse {
    match turn {
        ScriptedTurn::Reply(events) => {
            MockHttpResponse::success_typed(sse_body(events), "text/event-stream")
        }
        ScriptedTurn::TransportFailure(message) => {
            MockHttpResponse::error(StatusCode::INTERNAL_SERVER_ERROR, message.clone())
        }
    }
}

/// 事件序列编码为 OpenAI Chat SSE 帧。
fn sse_body(events: &[ScriptedEvent]) -> String {
    let mut body = String::new();
    let mut calls = 0_usize;
    for event in events {
        match event {
            ScriptedEvent::Token(text) => push_chunk(&mut body, &json!({ "content": text }), None),
            ScriptedEvent::ToolCall {
                id,
                tool,
                arguments,
            } => {
                let index = calls;
                calls += 1;
                push_chunk(
                    &mut body,
                    &json!({
                        "tool_calls": [{
                            "index": index,
                            "id": id,
                            "type": "function",
                            "function": { "name": tool, "arguments": arguments.to_string() },
                        }],
                    }),
                    None,
                );
            }
        }
    }
    let finish = if calls == 0 { "stop" } else { "tool_calls" };
    push_chunk(&mut body, &json!({}), Some(finish));
    body.push_str("data: [DONE]\n\n");
    body
}

/// 追写一个 `chat.completion.chunk` 帧。
fn push_chunk(body: &mut String, delta: &serde_json::Value, finish: Option<&str>) {
    let chunk = json!({
        "id": "chatcmpl-scripted",
        "object": "chat.completion.chunk",
        "created": 0,
        "model": SCRIPTED_MODEL,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
    });
    body.push_str("data: ");
    body.push_str(&chunk.to_string());
    body.push_str("\n\n");
}
