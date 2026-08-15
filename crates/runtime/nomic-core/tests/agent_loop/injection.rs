//! 运行中注入（turn 边界转向，ADR-0013/0014）集成测试
//! （自 `main.rs` 拆出的子模块）。
//!
//! 注入源（[`nomic_core::TurnInjection`]）由交互端实现；这里用一个测试
//! 专用注入源（共享队列 + 可选冻结）驱动 core loop，验证 turn 边界注入
//! 的契约——core 不拥有队列，只询问注入源。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use nomic_ai::{ImageContent, Message, StopReason, TextContent};
use nomic_core::{
    Agent, AgentEvent, AgentTool, DynTool, ToolError, ToolResult, TurnInjection, TurnMessage,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;

use super::support::{
    EchoParams, EchoTool, MockProvider, collect_events, error_done, model, text_done,
    tool_call_done,
};

// ── 测试专用注入源（共享队列 + 可选冻结）────────────────────────────────

/// 交互端注入源的替身：与 TUI 的统一消息队列同构（`Arc<Mutex<VecDeque>>`
/// 加冻结标志）。冻结期 [`TurnInjection::next_message`] 返回 `None`（run 可
/// 正常结束、队列保留），解冻后恢复弹出。
#[derive(Clone, Default)]
struct TestInjection {
    queue: Arc<Mutex<VecDeque<TurnMessage>>>,
    frozen: Arc<AtomicBool>,
}

impl TestInjection {
    fn push(&self, message: TurnMessage) {
        self.queue.lock().expect("lock").push_back(message);
    }

    fn freeze(&self) {
        self.frozen.store(true, Ordering::Relaxed);
    }

    fn unfreeze(&self) {
        self.frozen.store(false, Ordering::Relaxed);
    }

    fn is_empty(&self) -> bool {
        self.queue.lock().expect("lock").is_empty()
    }

    fn len(&self) -> usize {
        self.queue.lock().expect("lock").len()
    }

    fn pop_front(&self) -> Option<TurnMessage> {
        self.queue.lock().expect("lock").pop_front()
    }
}

impl TurnInjection for TestInjection {
    fn next_message(&self) -> Option<TurnMessage> {
        if self.frozen.load(Ordering::Relaxed) {
            return None;
        }
        self.queue.lock().expect("lock").pop_front()
    }
}

fn message(text: &str) -> TurnMessage {
    TurnMessage {
        text: text.to_string(),
        images: Vec::new(),
    }
}

fn make_agent_with_injection(
    provider: Arc<MockProvider>,
    tools: Vec<DynTool>,
    injection: Arc<dyn TurnInjection>,
) -> (Agent, UnboundedReceiver<AgentEvent>) {
    Agent::builder()
        .model(model())
        .provider(provider)
        .system_prompt("test system prompt")
        .tools(tools)
        .turn_injection(injection)
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build()
}

// ── 运行中注入（turn 边界转向）──────────────────────────────────────────

/// 门控工具：开始执行即通知测试，随后阻塞直到测试放行——
/// 用于在工具执行中途（run 进行中）确定性地入队注入消息。
struct GateTool {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl AgentTool for GateTool {
    type Params = EchoParams;

    fn name(&self) -> &'static str {
        "gate"
    }

    fn label(&self) -> &'static str {
        "gate"
    }

    fn description(&self) -> &'static str {
        "Block until released by the test."
    }

    async fn execute(
        &self,
        _params: Self::Params,
        _cancel: CancellationToken,
        _on_update: nomic_core::ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(ToolResult::text("released"))
    }
}

/// 工具执行中途入队的消息在当前 turn 的工具调用完成后、下一次 LLM 调用
/// 前注入：作为 user 消息进入历史与本次新增，run 继续。
#[tokio::test]
async fn injection_pushed_mid_run_injected_at_turn_boundary() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "gate", serde_json::json!({"text": "x"})),
        text_done("done"),
    ]);
    let source = TestInjection::default();
    let (mut agent, rx) = make_agent_with_injection(
        provider.clone(),
        vec![DynTool::new(GateTool {
            started: started.clone(),
            release: release.clone(),
        })],
        Arc::new(source.clone()),
    );

    let collector = tokio::spawn(collect_events(rx));
    let run = tokio::spawn(async move {
        agent
            .prompt("hi", CancellationToken::new())
            .await
            .expect("prompt")
    });
    // 等工具开始执行（turn 1 进入工具阶段），在工具完成前入队
    started.notified().await;
    source.push(message("顺便把测试也补上"));
    release.notify_one();
    let new_messages = run.await.expect("run");
    collector.await.expect("collector");

    // 历史：user → assistant(toolcall) → toolResult → user(注入) → assistant
    let kinds: Vec<&str> = new_messages
        .iter()
        .map(|m| match m {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        })
        .collect();
    assert_eq!(
        kinds,
        ["user", "assistant", "toolResult", "user", "assistant"]
    );
    let Message::User(injected) = &new_messages[3] else {
        panic!("第四条应为注入的 user 消息");
    };
    assert_eq!(
        injected.content,
        nomic_ai::UserMessageContent::Text("顺便把测试也补上".to_string())
    );
    // 第二次 LLM 调用的上下文已含注入消息
    assert_eq!(provider.context_lens(), vec![1, 4]);
    assert!(source.is_empty(), "注入后注入源已排空");
}

/// one-at-a-time：每个完成的 turn 投递一条；模型无工具调用但注入源未
/// 清空时 run 不结束，继续注入续行直至排空。
#[tokio::test]
async fn injection_one_at_a_time_keeps_run_alive_until_drained() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "x"})),
        text_done("a"),
        text_done("b"),
    ]);
    let source = TestInjection::default();
    let (mut agent, rx) = make_agent_with_injection(
        provider.clone(),
        vec![DynTool::new(EchoTool)],
        Arc::new(source.clone()),
    );
    source.push(message("第一条注入"));
    source.push(message("第二条注入"));

    let collector = tokio::spawn(collect_events(rx));
    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");
    collector.await.expect("collector");

    // user → assistant(toolcall) → toolResult → u1 → assistant(a) → u2 → assistant(b)
    assert_eq!(new_messages.len(), 7);
    assert_eq!(agent.messages().len(), 7);
    let Message::User(first) = &new_messages[3] else {
        panic!("第四条应为第一条注入");
    };
    let Message::User(second) = &new_messages[5] else {
        panic!("第六条应为第二条注入");
    };
    assert_eq!(
        first.content,
        nomic_ai::UserMessageContent::Text("第一条注入".to_string())
    );
    assert_eq!(
        second.content,
        nomic_ai::UserMessageContent::Text("第二条注入".to_string())
    );
    // 三次 LLM 调用：1（初始）→ 4（+toolcall/toolResult/u1）→ 6（+assistant/u2）
    assert_eq!(provider.context_lens(), vec![1, 4, 6]);
    assert!(source.is_empty());
}

/// 携带图片附件的注入消息：与 prompt 附件同一口径，图片块在前、文本块在后。
#[tokio::test]
async fn injection_with_images_builds_blocks_message() {
    let provider = MockProvider::new(vec![text_done("t1"), text_done("t2")]);
    let source = TestInjection::default();
    let (mut agent, _rx) = make_agent_with_injection(provider, vec![], Arc::new(source.clone()));
    let image = ImageContent {
        data: "aGVsbG8=".to_string(),
        mime_type: "image/png".to_string(),
    };
    source.push(TurnMessage {
        text: "看这张图".to_string(),
        images: vec![image.clone()],
    });

    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    // 首轮无工具调用，但注入源未清空 → 注入续行：user, asst, u(注入), asst
    assert_eq!(new_messages.len(), 4);
    let Message::User(injected) = &new_messages[2] else {
        panic!("第三条应为注入的 user 消息");
    };
    assert_eq!(
        injected.content,
        nomic_ai::UserMessageContent::Blocks(vec![
            nomic_ai::UserContent::Image(image),
            nomic_ai::UserContent::Text(TextContent {
                text: "看这张图".to_string(),
                text_signature: None,
            }),
        ])
    );
}

/// 响应以 Error 收尾时不询问注入源，注入消息保留（失败恢复由用户主导）。
#[tokio::test]
async fn error_turn_does_not_consume_injection() {
    let provider = MockProvider::new(vec![error_done(StopReason::Error, "boom")]);
    let source = TestInjection::default();
    let (mut agent, _rx) = make_agent_with_injection(provider, vec![], Arc::new(source.clone()));
    source.push(message("转向"));

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    assert_eq!(source.len(), 1, "异常收尾保留注入消息");
    assert_eq!(agent.messages().len(), 2, "历史不含注入消息");
}

/// 注入源返回 `None`（如冻结期）时 run 可正常结束，消息保留在源中；
/// 解冻后可再次弹出——core 只关心 `None`/`Some`，冻结语义归实现方。
#[tokio::test]
async fn source_returning_none_ends_run_and_retains_message() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "x"})),
        text_done("done"),
    ]);
    let source = TestInjection::default();
    let (mut agent, _rx) = make_agent_with_injection(
        provider.clone(),
        vec![DynTool::new(EchoTool)],
        Arc::new(source.clone()),
    );
    source.push(message("转向"));
    source.freeze();

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    // 冻结：两次 LLM 调用（toolcall turn + 收尾 turn），无注入
    assert_eq!(provider.context_lens(), vec![1, 3]);
    assert_eq!(agent.messages().len(), 4);
    assert_eq!(source.len(), 1);
    source.unfreeze();
    let popped = source.pop_front().expect("解冻后可弹出");
    assert_eq!(popped.text, "转向");
}

/// builder 接受共享注入源：agent 消费的就是外部持有的同一份注入源。
#[tokio::test]
async fn builder_accepts_shared_injection_source() {
    let source = TestInjection::default();
    let provider = MockProvider::new(vec![text_done("t1"), text_done("t2")]);
    let (mut agent, _rx) = make_agent_with_injection(provider, vec![], Arc::new(source.clone()));

    source.push(message("外部入队"));
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    assert!(source.is_empty(), "agent 消费的是同一份注入源");
}
