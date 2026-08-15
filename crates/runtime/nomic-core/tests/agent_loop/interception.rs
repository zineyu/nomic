//! 事件拦截（event interception）集成测试（自 `main.rs` 拆出的子模块）。
//!
//! 验证多拦截器的插入序、门控短路（deny-wins）与改写 pipeline。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nomic_ai::{Message, TextContent};
use nomic_core::{
    Agent, AgentInterceptor, DynTool, ToolCallDecision, ToolExecutionEnd, ToolExecutionOverride,
    ToolExecutionStart,
};
use tokio_util::sync::CancellationToken;

use super::support::{EchoTool, MockProvider, collect_events, model, text_done, tool_call_done};

struct BlockAllInterceptor;

#[async_trait]
impl AgentInterceptor for BlockAllInterceptor {
    async fn on_tool_execution_start(&self, _event: &ToolExecutionStart<'_>) -> ToolCallDecision {
        ToolCallDecision::Block {
            reason: "blocked by policy".to_string(),
        }
    }
}

#[tokio::test]
async fn interceptor_block_produces_error_result_without_executing() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "x"})),
        text_done("ok"),
    ]);
    let (mut agent, rx) = Agent::builder()
        .model(model())
        .provider(provider)
        .system_prompt("sys")
        .tools(vec![DynTool::new(EchoTool)])
        .interceptor(Arc::new(BlockAllInterceptor))
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build();

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    let Message::ToolResult(result) = &new_messages[2] else {
        panic!("expected tool result")
    };
    assert!(result.is_error);
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(text.text, "blocked by policy");
}

// ── 多拦截器语义 ──────────────────────────────────────────────────────────

/// 记录门控调用顺序，并返回固定决策。
struct GatingProbe {
    name: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
    decision: ToolCallDecision,
}

#[async_trait]
impl AgentInterceptor for GatingProbe {
    async fn on_tool_execution_start(&self, _event: &ToolExecutionStart<'_>) -> ToolCallDecision {
        self.calls.lock().expect("lock").push(self.name);
        self.decision.clone()
    }
}

#[tokio::test]
async fn interceptors_run_in_insertion_order() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (mut agent, rx) = Agent::builder()
        .model(model())
        .provider(MockProvider::new(vec![
            tool_call_done("c1", "echo", serde_json::json!({"text": "hi"})),
            text_done("ok"),
        ]))
        .system_prompt("sys")
        .tools(vec![DynTool::new(EchoTool)])
        .interceptor(Arc::new(GatingProbe {
            name: "a",
            calls: calls.clone(),
            decision: ToolCallDecision::Allow,
        }))
        .interceptor(Arc::new(GatingProbe {
            name: "b",
            calls: calls.clone(),
            decision: ToolCallDecision::Allow,
        }))
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build();

    let collector = tokio::spawn(collect_events(rx));
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    assert_eq!(*calls.lock().expect("lock"), vec!["a", "b"]);
}

#[tokio::test]
async fn interceptor_gating_short_circuits_on_first_block() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (mut agent, rx) = Agent::builder()
        .model(model())
        .provider(MockProvider::new(vec![
            tool_call_done("c1", "echo", serde_json::json!({"text": "hi"})),
            text_done("ok"),
        ]))
        .system_prompt("sys")
        .tools(vec![DynTool::new(EchoTool)])
        .interceptor(Arc::new(GatingProbe {
            name: "a",
            calls: calls.clone(),
            decision: ToolCallDecision::Block {
                reason: "blocked by a".to_string(),
            },
        }))
        .interceptor(Arc::new(GatingProbe {
            name: "b",
            calls: calls.clone(),
            decision: ToolCallDecision::Block {
                reason: "blocked by b".to_string(),
            },
        }))
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build();

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    // 首个 Block 短路：b 未被调用
    assert_eq!(*calls.lock().expect("lock"), vec!["a"]);

    let Message::ToolResult(result) = &new_messages[2] else {
        panic!("expected tool result")
    };
    assert!(result.is_error);
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(text.text, "blocked by a");
}

/// 记录 `on_tool_execution_end` 看到的累积内容，并可改写为固定内容。
struct RewriteProbe {
    seen: Arc<Mutex<Vec<String>>>,
    rewrite_to: Option<String>,
}

#[async_trait]
impl AgentInterceptor for RewriteProbe {
    async fn on_tool_execution_end(
        &self,
        event: &ToolExecutionEnd<'_>,
    ) -> Option<ToolExecutionOverride> {
        let seen: Vec<String> = event
            .result
            .content
            .iter()
            .map(|block| match block {
                nomic_ai::UserContent::Text(text) => text.text.clone(),
                nomic_ai::UserContent::Image(_) => "non-text".to_string(),
            })
            .collect();
        self.seen.lock().expect("lock").push(seen.join("|"));
        self.rewrite_to.clone().map(|text| ToolExecutionOverride {
            content: Some(vec![nomic_ai::UserContent::Text(TextContent {
                text,
                text_signature: None,
            })]),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn interceptor_rewrite_is_pipeline() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (mut agent, rx) = Agent::builder()
        .model(model())
        .provider(MockProvider::new(vec![
            tool_call_done("c1", "echo", serde_json::json!({"text": "hi"})),
            text_done("ok"),
        ]))
        .system_prompt("sys")
        .tools(vec![DynTool::new(EchoTool)])
        .interceptor(Arc::new(RewriteProbe {
            seen: seen.clone(),
            rewrite_to: Some("a-rewrite".to_string()),
        }))
        .interceptor(Arc::new(RewriteProbe {
            seen: seen.clone(),
            rewrite_to: Some("b-rewrite".to_string()),
        }))
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build();

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    // pipeline：a 看到原始 "hi"，b 看到 a 改写后的 "a-rewrite"
    assert_eq!(
        *seen.lock().expect("lock"),
        vec!["hi".to_string(), "a-rewrite".to_string()]
    );

    let Message::ToolResult(result) = &new_messages[2] else {
        panic!("expected tool result")
    };
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(text.text, "b-rewrite");
}
