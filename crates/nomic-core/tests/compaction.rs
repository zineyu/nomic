//! compaction 集成测试：录制上下文的 mock provider 验证自动/手动压缩行为。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use nomic_ai::{
    ApiKind, AssistantContent, AssistantEvent, AssistantMessage, Context, Message, Model, Provider,
    StopReason, StreamOptions, TextContent, Usage, now_millis,
};
use nomic_core::{Agent, AgentEvent, CompactionError, CompactionSettings, is_summary_message};
use tokio_util::sync::CancellationToken;

// ── 录制上下文的 mock provider ──────────────────────────────────────────────

struct RecordingProvider {
    /// 每次 stream 调用弹出一段事件脚本
    scripts: Mutex<VecDeque<Vec<AssistantEvent>>>,
    /// 每次 stream 调用收到的完整上下文
    contexts: Mutex<Vec<Context>>,
}

impl RecordingProvider {
    fn new(scripts: Vec<Vec<AssistantEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            contexts: Mutex::new(Vec::new()),
        })
    }

    fn contexts(&self) -> Vec<Context> {
        self.contexts.lock().expect("lock").clone()
    }
}

impl Provider for RecordingProvider {
    fn stream(
        &self,
        _model: &Model,
        context: &Context,
        _options: &StreamOptions,
        _cancel: CancellationToken,
    ) -> nomic_ai::AssistantStream {
        self.contexts.lock().expect("lock").push(context.clone());
        let events = self
            .scripts
            .lock()
            .expect("lock")
            .pop_front()
            .expect("no scripted response left");
        let (tx, stream) = nomic_ai::channel();
        tokio::spawn(async move {
            for event in events {
                let _ = tx.send(event);
            }
        });
        stream
    }
}

fn model(context_window: u64) -> Model {
    Model {
        id: "mock-model".to_string(),
        name: "mock".to_string(),
        api: ApiKind::OpenAiCompletions,
        provider: "mock".to_string(),
        base_url: "http://localhost".to_string(),
        reasoning: false,
        context_window,
        max_tokens: 4096,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    }
}

fn user(text: &str) -> Message {
    Message::User(nomic_ai::UserMessage {
        content: nomic_ai::UserMessageContent::Text(text.to_string()),
        timestamp: now_millis(),
    })
}

fn assistant(text: &str, total_tokens: u64) -> Message {
    Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::Text(TextContent {
            text: text.to_string(),
            text_signature: None,
        })],
        api: ApiKind::OpenAiCompletions,
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage {
            total_tokens,
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: now_millis(),
    })
}

fn text_done(text: &str) -> Vec<AssistantEvent> {
    vec![
        AssistantEvent::Start,
        AssistantEvent::TextStart { index: 0 },
        AssistantEvent::TextDelta {
            index: 0,
            delta: text.to_string(),
        },
        AssistantEvent::TextEnd { index: 0 },
        AssistantEvent::Done {
            message: Box::new(AssistantMessage {
                content: vec![AssistantContent::Text(TextContent {
                    text: text.to_string(),
                    text_signature: None,
                })],
                api: ApiKind::OpenAiCompletions,
                provider: "mock".to_string(),
                model: "mock-model".to_string(),
                response_model: None,
                response_id: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                error_message: None,
                timestamp: now_millis(),
            }),
        },
    ]
}

fn error_events(message: &str) -> Vec<AssistantEvent> {
    vec![
        AssistantEvent::Start,
        AssistantEvent::Error {
            message: Box::new(AssistantMessage {
                content: Vec::new(),
                api: ApiKind::OpenAiCompletions,
                provider: "mock".to_string(),
                model: "mock-model".to_string(),
                response_model: None,
                response_id: None,
                usage: Usage::default(),
                stop_reason: StopReason::Error,
                error_message: Some(message.to_string()),
                timestamp: now_millis(),
            }),
        },
    ]
}

fn make_agent(
    provider: Arc<RecordingProvider>,
    history: Vec<Message>,
    context_window: u64,
    compaction: CompactionSettings,
) -> (Agent, tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) {
    Agent::builder()
        .model(model(context_window))
        .provider(provider)
        .system_prompt("test system prompt")
        .messages(history)
        .compaction(compaction)
        .build()
}

async fn collect(mut rx: tokio::sync::mpsc::UnboundedReceiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        let end = matches!(event, AgentEvent::AgentEnd { .. });
        events.push(event);
        if end {
            break;
        }
    }
    events
}

fn user_text(message: &Message) -> &str {
    let Message::User(user) = message else {
        panic!("expected user message");
    };
    let nomic_ai::UserMessageContent::Text(text) = &user.content else {
        panic!("expected text content");
    };
    text
}

// ── 自动压缩 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn auto_compaction_triggers_between_turns_when_over_threshold() {
    // 历史：usage 锚点 2000 tokens；窗口 1000、reserve 100 → 阈值 900，必触发
    let history = vec![user("old question"), assistant("old answer", 2000)];
    let provider = RecordingProvider::new(vec![
        text_done("## Goal\nsummarized work"), // 摘要请求
        text_done("new answer"),               // 压缩后的正式 turn
    ]);
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 5,
    };
    let (mut agent, rx) = make_agent(provider.clone(), history, 1000, settings);
    let events = tokio::spawn(collect(rx));
    agent
        .prompt("new question", CancellationToken::new())
        .await
        .expect("prompt");
    let events = events.await.expect("collect");

    // 事件序列：CompactionStart/End 出现在 TurnStart 之前
    let position =
        |pred: &dyn Fn(&AgentEvent) -> bool| events.iter().position(pred).expect("event missing");
    let compaction_start = position(&|e| matches!(e, AgentEvent::CompactionStart { .. }));
    let compaction_end = position(&|e| matches!(e, AgentEvent::CompactionEnd { .. }));
    let turn_start = position(&|e| matches!(e, AgentEvent::TurnStart));
    assert!(compaction_start < compaction_end && compaction_end < turn_start);

    // CompactionEnd 携带摘要与保留条数（kept = assistant + 新 user = 2）
    let Some(AgentEvent::CompactionEnd {
        summary,
        kept_count,
        tokens_before,
        ..
    }) = events.get(compaction_end)
    else {
        panic!("compaction end event");
    };
    assert_eq!(summary, "## Goal\nsummarized work");
    assert_eq!(*kept_count, 2);
    // tokens_before = 锚点 2000 + 尾部新 user（"new question" 12 chars → 3）
    assert_eq!(*tokens_before, 2003);

    // 摘要请求与 agent 事件流隔离：无摘要的 MessageStart/MessageUpdate 混入
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::MessageStart(_)))
            .count(),
        2,
        "只有新 user 与正式 assistant 两条消息事件"
    );

    // provider 视角：第一次调用是摘要（1 条消息 + 摘要系统提示词），
    // 第二次是压缩后的正式请求（summary + kept 2 条 = 3 条）
    let contexts = provider.contexts();
    assert_eq!(contexts.len(), 2);
    assert_eq!(contexts[0].messages.len(), 1);
    assert!(
        contexts[0]
            .system_prompt
            .as_deref()
            .expect("system prompt")
            .contains("context summarization assistant")
    );
    let summary_request_text = user_text(&contexts[0].messages[0]);
    assert!(summary_request_text.contains("[User]: old question"));
    assert!(summary_request_text.contains("## Goal"));
    assert!(!summary_request_text.contains("<previous-summary>"));
    assert_eq!(contexts[1].messages.len(), 3);

    // agent 历史已替换：首条为合成摘要消息
    assert!(is_summary_message(&agent.messages()[0]));
    assert!(user_text(&agent.messages()[0]).contains("summarized work"));
}

#[tokio::test]
async fn auto_compaction_skipped_when_disabled() {
    let history = vec![user("old question"), assistant("old answer", 2000)];
    let provider = RecordingProvider::new(vec![text_done("new answer")]);
    let settings = CompactionSettings {
        enabled: false,
        ..CompactionSettings::default()
    };
    let (mut agent, rx) = make_agent(provider.clone(), history, 1000, settings);
    let events = tokio::spawn(collect(rx));
    agent
        .prompt("new question", CancellationToken::new())
        .await
        .expect("prompt");
    let events = events.await.expect("collect");

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::CompactionStart { .. }))
    );
    // 未压缩：正式请求携带全部历史
    assert_eq!(provider.contexts()[0].messages.len(), 3);
}

// ── 手动压缩 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn manual_compact_replaces_history_and_appends_file_lists() {
    let history = vec![
        user("please edit the code for me"),
        assistant_with_edit_call(),
        Message::ToolResult(nomic_ai::ToolResultMessage {
            tool_call_id: "c1".to_string(),
            tool_name: "edit".to_string(),
            content: vec![nomic_ai::UserContent::Text(TextContent {
                text: "ok".to_string(),
                text_signature: None,
            })],
            details: None,
            is_error: false,
            timestamp: now_millis(),
        }),
        // 尾部 user 足够大（40 chars = 10 tokens），使切点落在它前面，
        // 保证 edit 调用进入被摘要段
        user(&"n".repeat(40)),
    ];
    let provider = RecordingProvider::new(vec![text_done("## Goal\nedit code")]);
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 5,
    };
    let (mut agent, _rx) = make_agent(provider.clone(), history, 100_000, settings);

    let compaction = agent
        .compact(Some("focus on the edit"), CancellationToken::new())
        .await
        .expect("compact")
        .expect("something compacted");

    // 自定义指令进入摘要请求
    let contexts = provider.contexts();
    let request_text = user_text(&contexts[0].messages[0]);
    assert!(request_text.contains("Additional focus: focus on the edit"));
    // 文件操作确定性附加到摘要末尾
    assert!(
        compaction
            .summary
            .contains("<modified-files>\nsrc/lib.rs\n</modified-files>"),
        "{}",
        compaction.summary
    );
    assert!(is_summary_message(&agent.messages()[0]));
}

fn assistant_with_edit_call() -> Message {
    Message::Assistant(AssistantMessage {
        content: vec![AssistantContent::ToolCall(nomic_ai::ToolCall {
            id: "c1".to_string(),
            name: "edit".to_string(),
            arguments: serde_json::json!({"path": "src/lib.rs", "edits": []}),
            thought_signature: None,
        })],
        api: ApiKind::OpenAiCompletions,
        provider: "mock".to_string(),
        model: "mock-model".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
        error_message: None,
        timestamp: now_millis(),
    })
}

#[tokio::test]
async fn second_compaction_uses_update_prompt_with_previous_summary() {
    let history = vec![
        user(&"first task ".repeat(5)),
        assistant(&"first answer ".repeat(4), 100),
    ];
    let provider = RecordingProvider::new(vec![
        text_done("## Goal\nfirst summary"),
        text_done("## Goal\nupdated summary"),
    ]);
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 5,
    };
    let (mut agent, _rx) = make_agent(provider.clone(), history, 100_000, settings);

    agent
        .compact(None, CancellationToken::new())
        .await
        .expect("first compact")
        .expect("compacted");
    // 模拟压缩后继续对话
    agent.restore_messages(vec![
        agent.messages()[0].clone(),
        user(&"second task ".repeat(5)),
        assistant(&"second answer ".repeat(4), 100),
    ]);
    agent
        .compact(None, CancellationToken::new())
        .await
        .expect("second compact")
        .expect("compacted");

    // 第二次摘要走 UPDATE 变体并携带 <previous-summary>
    let contexts = provider.contexts();
    let second_request = user_text(&contexts[1].messages[0]);
    assert!(second_request.contains("<previous-summary>"));
    assert!(second_request.contains("first summary"));
    assert!(second_request.contains("NEW conversation messages"));
    // 前次摘要不重复出现在序列化对话里
    assert!(!second_request.contains("[User]: The conversation history before this point"));
}

#[tokio::test]
async fn compact_failure_keeps_history_and_returns_err() {
    let history = vec![
        user(&"u".repeat(40)),
        assistant(&"a".repeat(40), 100),
        user(&"u".repeat(40)),
        assistant(&"a".repeat(40), 100),
    ];
    let provider = RecordingProvider::new(vec![error_events("api down")]);
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 5,
    };
    let (mut agent, _rx) = make_agent(provider, history.clone(), 100_000, settings);

    let error = agent
        .compact(None, CancellationToken::new())
        .await
        .expect_err("摘要失败应返回 Err");
    assert!(matches!(error, CompactionError::Summarization(_)));
    assert_eq!(agent.messages(), history.as_slice(), "历史保持不变");
}

#[tokio::test]
async fn compact_returns_none_when_nothing_to_summarize() {
    let history = vec![user("hi"), assistant("hello", 100)];
    let provider = RecordingProvider::new(Vec::new());
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 100_000,
    };
    let (mut agent, _rx) = make_agent(provider, history, 100_000, settings);

    let result = agent
        .compact(None, CancellationToken::new())
        .await
        .expect("compact");
    assert!(result.is_none());
}
