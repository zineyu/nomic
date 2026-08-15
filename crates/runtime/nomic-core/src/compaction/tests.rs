//! 压缩算法的单元测试（自 `compaction.rs` 拆出的子模块，保持
//! `compaction.rs` 在行数上限内）。

use nomic_ai::{AssistantMessage, StopReason, TextContent, ToolCall, ToolResultMessage};

use super::*;

fn user(text: &str) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp: 0,
    })
}

fn assistant(blocks: Vec<AssistantContent>) -> Message {
    Message::Assistant(AssistantMessage {
        content: blocks,
        api: nomic_ai::ApiKind::OpenAiCompletions,
        provider: "mock".to_string(),
        model: "mock".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: 0,
    })
}

fn text_block(text: &str) -> AssistantContent {
    AssistantContent::Text(TextContent {
        text: text.to_string(),
        text_signature: None,
    })
}

fn tool_call_block(name: &str, args: serde_json::Value) -> AssistantContent {
    AssistantContent::ToolCall(ToolCall {
        id: "c1".to_string(),
        name: name.to_string(),
        arguments: args,
        thought_signature: None,
    })
}

fn tool_result(text: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        tool_call_id: "c1".to_string(),
        tool_name: "read".to_string(),
        content: vec![UserContent::Text(TextContent {
            text: text.to_string(),
            text_signature: None,
        })],
        details: None,
        is_error: false,
        timestamp: 0,
    })
}

fn with_usage(mut message: Message, total_tokens: u64) -> Message {
    if let Message::Assistant(assistant) = &mut message {
        assistant.usage.total_tokens = total_tokens;
    }
    message
}

// ── token 估算 ─────────────────────────────────────────────────────────

#[test]
fn estimate_context_tokens_falls_back_to_char_estimate_without_usage() {
    let messages = vec![user("12345678"), assistant(vec![text_block("1234")])];
    // 8/4 + 4/4 = 3
    assert_eq!(estimate_context_tokens(&messages), 3);
}

#[test]
fn estimate_context_tokens_anchors_at_last_valid_usage_plus_trailing() {
    let messages = vec![
        user("ignored"),
        with_usage(assistant(vec![text_block("x")]), 1000),
        user("12345678"),    // 2 tokens trailing
        tool_result("1234"), // 1 token trailing
    ];
    assert_eq!(estimate_context_tokens(&messages), 1003);
}

#[test]
fn estimate_context_tokens_skips_error_and_zero_usage_assistants() {
    let mut error_assistant = assistant(vec![text_block("x")]);
    if let Message::Assistant(a) = &mut error_assistant {
        a.stop_reason = StopReason::Error;
        a.usage.total_tokens = 5000;
    }
    let messages = vec![
        with_usage(assistant(vec![text_block("x")]), 100),
        error_assistant,
        user("12345678"), // 2 tokens trailing after the *valid* anchor
    ];
    // 锚点是第一条 assistant（error 的不算）；error assistant 按 chars 估算进 trailing
    // trailing = error assistant (1/4=0) + user (2) = 2
    assert_eq!(estimate_context_tokens(&messages), 102);
}

#[test]
fn should_compact_respects_threshold_window_and_enabled() {
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 10,
    };
    assert!(should_compact(901, 1000, &settings));
    assert!(!should_compact(900, 1000, &settings));
    assert!(!should_compact(5000, 0, &settings), "窗口未知不触发");
    // 窗口小于预留时阈值饱和为 0：有内容即触发（不能下溢）
    assert!(should_compact(
        5000,
        8192,
        &CompactionSettings {
            reserve_tokens: 16_384,
            ..settings
        }
    ));
    let disabled = CompactionSettings {
        enabled: false,
        ..settings
    };
    assert!(!should_compact(5000, 1000, &disabled));
}

// ── 切点 ───────────────────────────────────────────────────────────────

#[test]
fn cut_point_never_lands_on_tool_result() {
    // 每条消息 40 chars = 10 tokens；keep_recent=25 需要保留 3 条
    let messages = vec![
        user(&"u".repeat(40)),
        assistant(vec![text_block(&"a".repeat(40))]),
        tool_result(&"r".repeat(40)),
        assistant(vec![text_block(&"a".repeat(40))]),
        tool_result(&"r".repeat(40)),
    ];
    let cut = find_cut_point(&messages, 0, 25).expect("cut");
    assert!(
        matches!(messages[cut], Message::User(_) | Message::Assistant(_)),
        "切点必须在 user/assistant 边界"
    );
    // 累计到最后一条 assistant（含）时 10+10+10=30 >= 25，切在它前面
    assert_eq!(cut, 3);
}

#[test]
fn cut_point_returns_none_when_nothing_to_summarize() {
    let messages = vec![user("short"), assistant(vec![text_block("reply")])];
    // 总估算远低于预算时切点退化为 start → None
    assert_eq!(find_cut_point(&messages, 0, 10_000), None);
}

#[test]
fn cut_point_skips_summary_prefix_offset() {
    let summary = summary_message("previous work", 0);
    let messages = vec![
        summary,
        user(&"u".repeat(400)),                        // 100 tokens
        assistant(vec![text_block(&"a".repeat(400))]), // 100 tokens
        user(&"u".repeat(400)),                        // 100 tokens
    ];
    // start=1；keep 150 → 从尾部累计第二条 user 处达 200，切在 index 2
    let cut = find_cut_point(&messages, 1, 150).expect("cut");
    assert_eq!(cut, 2);
    // start 之后无内容可摘要时 None
    assert_eq!(find_cut_point(&messages[..2], 1, 150), None);
}

// ── 序列化 ─────────────────────────────────────────────────────────────

#[test]
fn serialize_conversation_formats_each_role() {
    let messages = vec![
        user("hello"),
        assistant(vec![
            AssistantContent::Thinking(nomic_ai::ThinkingContent {
                thinking: "hmm".to_string(),
                thinking_signature: None,
                redacted: false,
            }),
            text_block("let me read"),
            tool_call_block("read", serde_json::json!({"path": "a.rs", "offset": 1})),
            tool_call_block("bash", serde_json::json!({"command": "ls"})),
        ]),
        tool_result("file contents"),
    ];
    let text = serialize_conversation(&messages);
    assert!(text.contains("[User]: hello"), "{text}");
    assert!(text.contains("[Assistant thinking]: hmm"), "{text}");
    assert!(text.contains("[Assistant]: let me read"), "{text}");
    assert!(
        text.contains(
            "[Assistant tool calls]: read(offset=1, path=\"a.rs\"); bash(command=\"ls\")"
        ),
        "{text}"
    );
    assert!(text.contains("[Tool result]: file contents"), "{text}");
}

#[test]
fn serialize_truncates_tool_results_with_marker() {
    let long = "x".repeat(3000);
    let text = serialize_conversation(&[tool_result(&long)]);
    assert!(
        text.contains("[... 1000 more characters truncated]"),
        "{text}"
    );
    assert!(!text.contains(&"x".repeat(2001)));
}

// ── 文件操作提取 ─────────────────────────────────────────────────────────

#[test]
fn file_ops_from_tool_calls_and_previous_summary() {
    let messages = vec![assistant(vec![
        tool_call_block("read", serde_json::json!({"path": "src/a.rs"})),
        tool_call_block("read", serde_json::json!({"path": "src/b.rs"})),
        tool_call_block("edit", serde_json::json!({"path": "src/b.rs"})),
        tool_call_block("write", serde_json::json!({"path": "src/c.rs"})),
        tool_call_block("bash", serde_json::json!({"command": "touch x"})),
    ])];
    let previous = "text\n<read-files>\nsrc/old.rs\nsrc/b.rs\n</read-files>\n<modified-files>\nsrc/d.rs\n</modified-files>";
    let ops = extract_file_ops(&messages, Some(previous));
    // b.rs 被修改 → 从只读剔除；bash 无 path 语义；前次清单合并
    let read: Vec<_> = ops.read_files.iter().cloned().collect();
    let modified: Vec<_> = ops.modified_files.iter().cloned().collect();
    assert_eq!(read, vec!["src/a.rs", "src/old.rs"]);
    assert_eq!(modified, vec!["src/b.rs", "src/c.rs", "src/d.rs"]);

    let formatted = format_file_ops(&ops);
    assert!(formatted.contains("<read-files>\nsrc/a.rs\nsrc/old.rs\n</read-files>"));
    assert!(formatted.contains("<modified-files>"));
    assert!(format_file_ops(&FileOps::default()).is_empty());
}
