//! fixture 回放测试：验证 OpenAI SSE 流到 `AssistantEvent` 协议的映射。

use std::convert::Infallible;

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::types::ApiKind;

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
        api: ApiKind::OpenAiCompletions,
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
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

const TEXT_AND_TOOL_CALLS: &str = r#"
data: {"id":"chatcmpl-1","model":"gpt-test","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"chatcmpl-1","model":"gpt-test","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"chatcmpl-1","model":"gpt-test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"pa"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-1","model":"gpt-test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-1","model":"gpt-test","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"id":"chatcmpl-1","model":"gpt-test","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":8,"total_tokens":18}}

data: [DONE]
"#;

#[tokio::test]
async fn text_and_tool_calls_stream() {
    let (events, message) = run_fixture(TEXT_AND_TOOL_CALLS).await;

    assert!(matches!(events[0], AssistantEvent::Start));
    assert!(matches!(events[1], AssistantEvent::TextStart { index: 0 }));
    assert!(
        matches!(&events[2], AssistantEvent::TextDelta { index: 0, delta } if delta == "Hello")
    );
    assert!(matches!(
        events[3],
        AssistantEvent::ToolCallStart { index: 1 }
    ));
    match events.last() {
        Some(AssistantEvent::ToolCallEnd {
            index: 1,
            tool_call,
        }) => {
            assert_eq!(tool_call.id, "call_1");
            assert_eq!(tool_call.name, "read");
            assert_eq!(tool_call.arguments, serde_json::json!({ "path": "a.txt" }));
        }
        other => panic!("expected ToolCallEnd last, got {other:?}"),
    }

    assert_eq!(message.stop_reason, StopReason::ToolUse);
    assert_eq!(message.usage.input, 10);
    assert_eq!(message.usage.output, 8);
    assert_eq!(message.usage.total_tokens, 18);
    let AssistantContent::Text(text) = &message.content[0] else {
        panic!("expected text block")
    };
    assert_eq!(text.text, "Hello");
}

const REASONING_CONTENT: &str = r#"
data: {"id":"chatcmpl-2","model":"deepseek-test","choices":[{"index":0,"delta":{"reasoning_content":"thinking hard"},"finish_reason":null}]}

data: {"id":"chatcmpl-2","model":"deepseek-test","choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":null}]}

data: {"id":"chatcmpl-2","model":"deepseek-test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: {"id":"chatcmpl-2","model":"deepseek-test","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":7,"total_tokens":12,"completion_tokens_details":{"reasoning_tokens":3}}}

data: [DONE]
"#;

#[tokio::test]
async fn reasoning_content_maps_to_thinking_block() {
    let (events, message) = run_fixture(REASONING_CONTENT).await;

    assert!(matches!(
        events[1],
        AssistantEvent::ThinkingStart { index: 0 }
    ));
    assert!(matches!(events[3], AssistantEvent::TextStart { index: 1 }));

    let AssistantContent::Thinking(thinking) = &message.content[0] else {
        panic!("expected thinking block")
    };
    assert_eq!(thinking.thinking, "thinking hard");
    // signature 记录回放字段名
    assert_eq!(
        thinking.thinking_signature.as_deref(),
        Some("reasoning_content")
    );
    let AssistantContent::Text(text) = &message.content[1] else {
        panic!("expected text block")
    };
    assert_eq!(text.text, "answer");
    assert_eq!(message.usage.reasoning, Some(3));
    assert_eq!(message.stop_reason, StopReason::Stop);
}

const MULTIPLE_PARALLEL_TOOL_CALLS: &str = r#"
data: {"id":"chatcmpl-3","model":"gpt-test","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-3","model":"gpt-test","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"read","arguments":"{\"path\":"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-3","model":"gpt-test","choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"b\"}"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-3","model":"gpt-test","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;

#[tokio::test]
async fn parallel_tool_calls_accumulate_by_index() {
    let (events, message) = run_fixture(MULTIPLE_PARALLEL_TOOL_CALLS).await;

    let ends: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AssistantEvent::ToolCallEnd { tool_call, .. } => Some(tool_call),
            _ => None,
        })
        .collect();
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[0].name, "read");
    assert_eq!(ends[0].arguments, serde_json::json!({ "path": "a" }));
    assert_eq!(ends[1].arguments, serde_json::json!({ "path": "b" }));
    assert_eq!(message.content.len(), 2);
}

#[test]
fn request_converts_assistant_tool_calls_and_tool_results() {
    let context = Context {
        system_prompt: Some("sys".to_string()),
        messages: vec![
            Message::Assistant(AssistantMessage {
                content: vec![
                    AssistantContent::Thinking(ThinkingContent {
                        thinking: "hmm".to_string(),
                        thinking_signature: Some("reasoning_content".to_string()),
                        redacted: false,
                    }),
                    AssistantContent::Text(TextContent {
                        text: "reading".to_string(),
                        text_signature: None,
                    }),
                    AssistantContent::ToolCall(ToolCall {
                        id: "call_1".to_string(),
                        name: "read".to_string(),
                        arguments: serde_json::json!({"path": "a"}),
                        thought_signature: None,
                    }),
                ],
                api: ApiKind::OpenAiCompletions,
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                response_model: None,
                response_id: None,
                usage: crate::types::Usage::default(),
                stop_reason: StopReason::ToolUse,
                error_message: None,
                timestamp: 0,
            }),
            Message::ToolResult(ToolResultMessage {
                tool_call_id: "call_1".to_string(),
                tool_name: "read".to_string(),
                content: vec![UserContent::Text(TextContent {
                    text: "file a".to_string(),
                    text_signature: None,
                })],
                details: None,
                is_error: false,
                timestamp: 0,
            }),
        ],
        tools: Vec::new(),
    };
    let model = Model {
        id: "gpt-test".to_string(),
        name: "test".to_string(),
        api: ApiKind::OpenAiCompletions,
        provider: "openai".to_string(),
        base_url: "https://api.openai.com/v1".to_string(),
        reasoning: false,
        context_window: 128_000,
        max_tokens: 4096,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    };
    let body = build_request(
        &model,
        &context,
        &StreamOptions::default(),
        &OpenAiCompat::default(),
    );

    let messages = body["messages"].as_array().expect("messages array");
    assert_eq!(
        messages[0],
        serde_json::json!({ "role": "system", "content": "sys" })
    );
    // assistant：纯字符串 content + 回放 reasoning_content + tool_calls
    assert_eq!(messages[1]["content"], "reading");
    assert_eq!(messages[1]["reasoning_content"], "hmm");
    assert_eq!(
        messages[1]["tool_calls"][0]["function"]["arguments"],
        r#"{"path":"a"}"#
    );
    // 工具结果：独立的 role=tool 消息
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "call_1");
    assert_eq!(messages[2]["content"], "file a");
    assert_eq!(
        body["stream_options"],
        serde_json::json!({ "include_usage": true })
    );
}

// ── 失败重试的端到端测试（脚本化 HTTP 服务器）────────────────────────────────

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::providers::retry::{RetryPolicy, test_server};
use crate::types::Context;

/// 完整的成功 SSE 响应体（一个文本 delta + finish + usage + [DONE]）。
const SSE_OK: &str = concat!(
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-1\",\"model\":\"gpt-test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
    "data: [DONE]\n\n",
);

/// 退避基数压到 1ms 的 provider，避免测试真实睡眠。
fn fast_retry_provider() -> OpenAiProvider {
    let mut provider = OpenAiProvider::new(Some("test-key".to_string()), OpenAiCompat::default());
    provider.retry_policy = RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_millis(1),
    };
    provider
}

fn retry_model(base_url: String) -> Model {
    Model {
        id: "gpt-test".to_string(),
        name: "gpt-test".to_string(),
        api: ApiKind::OpenAiCompletions,
        provider: "openai".to_string(),
        base_url,
        reasoning: false,
        context_window: 128_000,
        max_tokens: 4096,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    }
}

fn empty_context() -> Context {
    Context {
        system_prompt: None,
        messages: vec![Message::User(crate::types::UserMessage {
            content: crate::types::UserMessageContent::Text("hello".to_string()),
            timestamp: 0,
        })],
        tools: Vec::new(),
    }
}

#[tokio::test]
async fn retries_transient_errors_until_success() {
    let (base_url, count) = test_server::start(vec![
        test_server::http_error(500, "Internal Server Error", "boom"),
        test_server::http_error(500, "Internal Server Error", "boom"),
        test_server::sse(SSE_OK),
    ]);
    let provider = fast_retry_provider();
    let stream = provider.stream(
        &retry_model(base_url),
        &empty_context(),
        &StreamOptions::default(),
        CancellationToken::new(),
    );

    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Stop);
    assert_eq!(count.load(Ordering::SeqCst), 3, "两次 500 后第三次成功");
    assert!(
        matches!(&message.content[0], AssistantContent::Text(t) if t.text == "hi"),
        "unexpected content: {:?}",
        message.content
    );
}

#[tokio::test]
async fn gives_up_after_three_retries() {
    let (base_url, count) = test_server::start(vec![
        test_server::http_error(500, "Internal Server Error", "boom"),
        test_server::http_error(500, "Internal Server Error", "boom"),
        test_server::http_error(500, "Internal Server Error", "boom"),
        test_server::http_error(500, "Internal Server Error", "boom"),
    ]);
    let provider = fast_retry_provider();
    let stream = provider.stream(
        &retry_model(base_url),
        &empty_context(),
        &StreamOptions::default(),
        CancellationToken::new(),
    );

    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(count.load(Ordering::SeqCst), 4, "首次 + 3 次重试后放弃");
    let error = message.error_message.expect("error message");
    assert!(error.contains("HTTP 500"), "unexpected error: {error}");
}

#[tokio::test]
async fn fatal_status_is_not_retried() {
    let (base_url, count) = test_server::start(vec![test_server::http_error(
        400,
        "Bad Request",
        "invalid request",
    )]);
    let provider = fast_retry_provider();
    let stream = provider.stream(
        &retry_model(base_url),
        &empty_context(),
        &StreamOptions::default(),
        CancellationToken::new(),
    );

    let message = stream.result().await;
    assert_eq!(message.stop_reason, StopReason::Error);
    assert_eq!(count.load(Ordering::SeqCst), 1, "4xx 不重试");
}
