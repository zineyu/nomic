//! steering 队列（ADR-0013，pi 式运行中转向）集成测试
//! （自 `agent_loop.rs` 拆出的子模块）。

use std::sync::Arc;

use async_trait::async_trait;
use nomic_ai::{Message, StopReason, TextContent};
use nomic_core::{Agent, AgentTool, DynTool, ToolError, ToolResult};
use tokio_util::sync::CancellationToken;

use super::support::{
    EchoParams, EchoTool, MockProvider, collect_events, error_done, make_agent, model, text_done,
    tool_call_done,
};

// ── steering 队列（ADR-0013，pi 式运行中转向）──────────────────────────────

/// 门控工具：开始执行即通知测试，随后阻塞直到测试放行——
/// 用于在工具执行中途（run 进行中）确定性地入队 steering。
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

/// 工具执行中途入队的 steering 在当前 turn 的工具调用完成后、下一次
/// LLM 调用前注入：作为 user 消息进入历史与本次新增，run 继续。
#[tokio::test]
async fn steering_pushed_mid_run_injected_at_turn_boundary() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "gate", serde_json::json!({"text": "x"})),
        text_done("done"),
    ]);
    let (mut agent, rx) = make_agent(
        provider.clone(),
        vec![DynTool::new(GateTool {
            started: started.clone(),
            release: release.clone(),
        })],
    );
    let steering = agent.steering_handle();

    let collector = tokio::spawn(collect_events(rx));
    let run = tokio::spawn(async move {
        agent
            .prompt("hi", CancellationToken::new())
            .await
            .expect("prompt")
    });
    // 等工具开始执行（turn 1 进入工具阶段），在工具完成前入队 steering
    started.notified().await;
    steering.push(nomic_core::SteeringMessage {
        text: "顺便把测试也补上".to_string(),
        images: Vec::new(),
    });
    release.notify_one();
    let new_messages = run.await.expect("run");
    collector.await.expect("collector");

    // 历史：user → assistant(toolcall) → toolResult → user(steering) → assistant
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
    let Message::User(steered) = &new_messages[3] else {
        panic!("第四条应为 steering 注入的 user 消息");
    };
    assert_eq!(
        steered.content,
        nomic_ai::UserMessageContent::Text("顺便把测试也补上".to_string())
    );
    // 第二次 LLM 调用的上下文已含 steering 消息
    assert_eq!(provider.context_lens(), vec![1, 4]);
    assert!(steering.is_empty(), "注入后队列已排空");
}

/// one-at-a-time：每个完成的 turn 投递一条；模型无工具调用但队列未
/// 清空时 run 不结束，继续注入续行直至排空。
#[tokio::test]
async fn steering_one_at_a_time_keeps_run_alive_until_drained() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "x"})),
        text_done("a"),
        text_done("b"),
    ]);
    let (mut agent, rx) = make_agent(provider.clone(), vec![DynTool::new(EchoTool)]);
    let steering = agent.steering_handle();
    steering.push(nomic_core::SteeringMessage {
        text: "第一条转向".to_string(),
        images: Vec::new(),
    });
    steering.push(nomic_core::SteeringMessage {
        text: "第二条转向".to_string(),
        images: Vec::new(),
    });

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
        panic!("第四条应为第一条 steering");
    };
    let Message::User(second) = &new_messages[5] else {
        panic!("第六条应为第二条 steering");
    };
    assert_eq!(
        first.content,
        nomic_ai::UserMessageContent::Text("第一条转向".to_string())
    );
    assert_eq!(
        second.content,
        nomic_ai::UserMessageContent::Text("第二条转向".to_string())
    );
    // 三次 LLM 调用：1（初始）→ 4（+toolcall/toolResult/u1）→ 6（+assistant/u2）
    assert_eq!(provider.context_lens(), vec![1, 4, 6]);
    assert!(steering.is_empty());
}

/// 携带图片附件的 steering：与 prompt 附件同一口径，图片块在前、文本块在后。
#[tokio::test]
async fn steering_with_images_builds_blocks_message() {
    let provider = MockProvider::new(vec![text_done("t1"), text_done("t2")]);
    let (mut agent, _rx) = make_agent(provider, vec![]);
    let steering = agent.steering_handle();
    let image = nomic_ai::ImageContent {
        data: "aGVsbG8=".to_string(),
        mime_type: "image/png".to_string(),
    };
    steering.push(nomic_core::SteeringMessage {
        text: "看这张图".to_string(),
        images: vec![image.clone()],
    });

    let new_messages = agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    // 首轮无工具调用，但 steering 未清空 → 注入续行：user, asst, u(steered), asst
    assert_eq!(new_messages.len(), 4);
    let Message::User(steered) = &new_messages[2] else {
        panic!("第三条应为 steering 注入的 user 消息");
    };
    assert_eq!(
        steered.content,
        nomic_ai::UserMessageContent::Blocks(vec![
            nomic_ai::UserContent::Image(image),
            nomic_ai::UserContent::Text(TextContent {
                text: "看这张图".to_string(),
                text_signature: None,
            }),
        ])
    );
}

/// 响应以 Error 收尾时不注入 steering，队列保留（失败恢复由用户主导）。
#[tokio::test]
async fn error_turn_does_not_drain_steering() {
    let provider = MockProvider::new(vec![error_done(StopReason::Error, "boom")]);
    let (mut agent, _rx) = make_agent(provider, vec![]);
    let steering = agent.steering_handle();
    steering.push(nomic_core::SteeringMessage {
        text: "转向".to_string(),
        images: Vec::new(),
    });

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    assert_eq!(steering.len(), 1, "异常收尾保留 steering 队列");
    assert_eq!(agent.messages().len(), 2, "历史不含 steering 消息");
}

/// 冻结期间（QUEUE 编辑）turn 边界不弹出 steering；run 可正常结束，
/// 解冻后队列内容不变。
#[tokio::test]
async fn frozen_steering_is_not_injected() {
    let provider = MockProvider::new(vec![
        tool_call_done("c1", "echo", serde_json::json!({"text": "x"})),
        text_done("done"),
    ]);
    let (mut agent, _rx) = make_agent(provider.clone(), vec![DynTool::new(EchoTool)]);
    let steering = agent.steering_handle();
    steering.push(nomic_core::SteeringMessage {
        text: "转向".to_string(),
        images: Vec::new(),
    });
    steering.freeze();

    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    // 冻结：两次 LLM 调用（toolcall turn + 收尾 turn），无 steering 注入
    assert_eq!(provider.context_lens(), vec![1, 3]);
    assert_eq!(agent.messages().len(), 4);
    assert_eq!(steering.len(), 1);
    steering.unfreeze();
    let popped = steering.pop_front().expect("解冻后可弹出");
    assert_eq!(popped.text, "转向");
}

/// 共享句柄：builder 注入的队列与 agent 内部是同一份（交互端直推语义）。
#[tokio::test]
async fn builder_accepts_shared_steering_queue() {
    let shared = nomic_core::SteeringQueue::new();
    let provider = MockProvider::new(vec![text_done("t1"), text_done("t2")]);
    let (mut agent, _rx) = Agent::builder()
        .model(model())
        .provider(provider)
        .system_prompt("test system prompt")
        .steering_queue(shared.clone())
        .compaction(nomic_core::CompactionSettings {
            enabled: false,
            ..Default::default()
        })
        .build();

    shared.push(nomic_core::SteeringMessage {
        text: "外部入队".to_string(),
        images: Vec::new(),
    });
    agent
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt");

    assert!(shared.is_empty(), "agent 消费的是同一份队列");
}
