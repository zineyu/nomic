//! fixture 回放测试：验证 Anthropic SSE 流到 `AssistantEvent` 协议的映射。

use std::convert::Infallible;

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::types::ApiKind;
use crate::{ToolResultMessage, UserMessage, UserMessageContent};

fn fixture_events(
    text: &'static str,
) -> BoxStream<'static, Result<Event, EventStreamError<Infallible>>> {
    // SSE 事件以空行分隔；末尾补一个空行确保最后一条事件被分发
    let bytes = format!("{text}\n").into_bytes();
    futures::stream::once(async move { Ok::<_, Infallible>(bytes) })
        .eventsource()
        .boxed()
}

async fn run_fixture(text: &'static str) -> (Vec<AssistantEvent>, AssistantMessage) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut output = AssistantMessage {
        content: Vec::new(),
        api: ApiKind::AnthropicMessages,
        provider: "anthropic".to_string(),
        model: "claude-test".to_string(),
        response_model: None,
        response_id: None,
        usage: crate::types::Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
    };
    let handle = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    });
    // 生产路径中 Start 由 HTTP 层在收到响应后发送，这里模拟同样的行为
    tx.send(AssistantEvent::Start).expect("receiver alive");
    let result = process_events(
        fixture_events(text),
        CancellationToken::new(),
        &mut output,
        &tx,
    )
    .await;
    drop(tx);
    assert!(
        result.is_ok(),
        "fixture should process cleanly: {:?}",
        result.err()
    );
    (handle.await.expect("collector panicked"), output)
}

const TEXT_AND_TOOL_USE: &str = r#"
event: message_start
data: {"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","model":"claude-test","usage":{"input_tokens":25,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"read","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}

event: message_stop
data: {"type":"message_stop"}
"#;

#[tokio::test]
async fn text_and_tool_use_stream() {
    let (events, message) = run_fixture(TEXT_AND_TOOL_USE).await;

    assert!(matches!(events[0], AssistantEvent::Start));
    assert!(matches!(events[1], AssistantEvent::TextStart { index: 0 }));
    assert!(
        matches!(&events[2], AssistantEvent::TextDelta { index: 0, delta } if delta == "Hello")
    );
    assert!(
        matches!(&events[3], AssistantEvent::TextDelta { index: 0, delta } if delta == " world")
    );
    assert!(matches!(events[4], AssistantEvent::TextEnd { index: 0 }));
    assert!(matches!(
        events[5],
        AssistantEvent::ToolCallStart { index: 1 }
    ));
    assert!(matches!(
        &events[6],
        AssistantEvent::ToolCallDelta { index: 1, .. }
    ));
    assert!(matches!(
        &events[7],
        AssistantEvent::ToolCallDelta { index: 1, .. }
    ));
    match &events[8] {
        AssistantEvent::ToolCallEnd {
            index: 1,
            tool_call,
        } => {
            assert_eq!(tool_call.id, "toolu_01");
            assert_eq!(tool_call.name, "read");
            assert_eq!(tool_call.arguments, serde_json::json!({ "path": "a.txt" }));
        }
        other => panic!("expected ToolCallEnd, got {other:?}"),
    }
    assert_eq!(events.len(), 9);

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.response_id.as_deref(), Some("msg_01"));
    assert_eq!(message.usage.input, 25);
    assert_eq!(message.usage.output, 12);
    let AssistantContent::Text(text) = &message.content[0] else {
        panic!("expected text block")
    };
    assert_eq!(text.text, "Hello world");
}

const THINKING_WITH_SIGNATURE: &str = r#"
event: message_start
data: {"type":"message_start","message":{"id":"msg_02","type":"message","role":"assistant","model":"claude-test","usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"let me think"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_abc"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}
"#;

#[tokio::test]
async fn thinking_block_keeps_signature() {
    let (events, message) = run_fixture(THINKING_WITH_SIGNATURE).await;

    assert!(matches!(
        events[1],
        AssistantEvent::ThinkingStart { index: 0 }
    ));
    assert!(
        matches!(&events[2], AssistantEvent::ThinkingDelta { index: 0, delta } if delta == "let me think")
    );
    assert!(matches!(
        events[3],
        AssistantEvent::ThinkingEnd { index: 0 }
    ));

    let AssistantContent::Thinking(thinking) = &message.content[0] else {
        panic!("expected thinking block")
    };
    assert_eq!(thinking.thinking, "let me think");
    assert_eq!(thinking.thinking_signature.as_deref(), Some("sig_abc"));
    assert_eq!(message.stop_reason, StopReason::Stop);
}

const TRUNCATED_TOOL_JSON: &str = r#"
event: message_start
data: {"type":"message_start","message":{"id":"msg_03","type":"message","role":"assistant","model":"claude-test","usage":{"input_tokens":10,"output_tokens":1}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_02","name":"write","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\",\"content\":\"hello wor"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":100}}

event: message_stop
data: {"type":"message_stop"}
"#;

#[tokio::test]
async fn truncated_tool_json_is_salvaged() {
    let (_, message) = run_fixture(TRUNCATED_TOOL_JSON).await;

    assert_eq!(message.stop_reason, StopReason::Length);
    let AssistantContent::ToolCall(call) = &message.content[0] else {
        panic!("expected tool call")
    };
    // 截断的 partial JSON 被修复为可解析对象
    assert_eq!(call.arguments["path"], "a.txt");
    assert_eq!(call.arguments["content"], "hello wor");
}

#[test]
fn request_converts_consecutive_tool_results_into_one_user_message() {
    let context = Context {
        system_prompt: Some("sys".to_string()),
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_string()),
                timestamp: 0,
            }),
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantContent::Text(TextContent {
                        text: "reading".to_string(),
                        text_signature: None,
                    }),
                    AssistantContent::ToolCall(ToolCall {
                        id: "t1".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": "a"}),
                        thought_signature: None,
                    }),
                ],
                api: ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude-test".to_string(),
                response_model: None,
                response_id: None,
                usage: crate::types::Usage::default(),
                stop_reason: StopReason::ToolUse,
                error_message: None,
                timestamp: 0,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "t1".to_string(),
                tool_name: "read".to_string(),
                content: vec![UserContent::Text(TextContent {
                    text: "file a".to_string(),
                    text_signature: None,
                })],
                details: None,
                is_error: false,
                timestamp: 0,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "t2".to_string(),
                tool_name: "read".to_string(),
                content: vec![UserContent::Text(TextContent {
                    text: "file b".to_string(),
                    text_signature: None,
                })],
                details: None,
                is_error: true,
                timestamp: 0,
            }),
        ],
        tools: Vec::new(),
    };
    let model = Model {
        id: "claude-test".to_string(),
        name: "test".to_string(),
        api: ApiKind::AnthropicMessages,
        provider: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        reasoning: false,
        context_window: 200_000,
        max_tokens: 8192,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    };
    let body = build_request(&model, &context, &StreamOptions::default());

    assert_eq!(
        body["system"],
        serde_json::json!([{ "type": "text", "text": "sys" }])
    );
    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 3);
    // 两条连续 toolResult 合并为一条 user 消息
    assert_eq!(messages[2]["role"], "user");
    let results = messages[2]["content"]
        .as_array()
        .expect("tool_result array");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "t1");
    assert_eq!(results[1]["is_error"], true);
}

#[test]
fn request_enables_thinking_with_budget() {
    let model = Model {
        id: "claude-thinking".to_string(),
        name: "test".to_string(),
        api: ApiKind::AnthropicMessages,
        provider: "anthropic".to_string(),
        base_url: "https://api.anthropic.com".to_string(),
        reasoning: true,
        context_window: 200_000,
        max_tokens: 8192,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    };
    let options = StreamOptions {
        reasoning: Some(ThinkingLevel::Low),
        ..StreamOptions::default()
    };
    let body = build_request(&model, &Context::default(), &options);
    assert_eq!(
        body["thinking"],
        serde_json::json!({ "type": "enabled", "budget_tokens": 2048 })
    );
}
