//! agent actor 集成测试（ADR-0022）：命令邮箱的回执、FIFO 顺序与断连语义。

// 与 agent_loop 共享 mock provider / 构造 helper；本测试只用其中一部分
#[allow(dead_code)]
#[path = "agent_loop/support.rs"]
mod support;

use std::sync::Arc;

use nomic_ai::{
    Context, Message, Model, Provider, StreamOptions, ThinkingLevel, UserMessage,
    UserMessageContent, now_millis,
};
use nomic_core::{ActorError, Agent, estimate_context_tokens};
use support::{MockProvider, assistant_message, collect_events, make_agent, model, text_done};
use tokio_util::sync::CancellationToken;

fn user_message(text: &str) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp: now_millis(),
    })
}

/// prompt 经邮箱回执返回；事件流与查询命令口径不变。
#[tokio::test]
async fn prompt_roundtrip_through_mailbox() {
    let provider = MockProvider::new(vec![text_done("hi")]);
    let (agent, events) = make_agent(provider.clone(), vec![]);
    let (handle, task) = agent.spawn();

    let new_messages = handle
        .prompt("hello", CancellationToken::new())
        .await
        .expect("prompt 应成功");
    assert_eq!(new_messages.len(), 2, "user + assistant");

    // prompt 完成后 AgentEnd 已在通道内，collect 不会因等待而挂起
    let events = collect_events(events).await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, nomic_core::AgentEvent::AgentEnd { .. })),
        "事件流应完整到达"
    );

    let history = handle.messages().expect("查询应成功");
    assert_eq!(history.len(), 2);
    assert_eq!(provider.context_lens(), vec![1], "上下文含注入的 user 消息");

    // 全部句柄断开后 actor 任务自然退出
    drop(handle);
    task.await.expect("actor 应正常退出");
}

/// fire-and-forget 变更按邮箱 FIFO 生效：紧随的 prompt 一定看到结果；
/// 查询读共享快照视图（ADR-0035），读己之写先经 `flush` 屏障。
#[tokio::test]
async fn mutations_apply_in_mailbox_order() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (agent, _events) = make_agent(provider.clone(), vec![]);
    let (handle, _task) = agent.spawn();

    handle.inject_user_message("第一条").expect("注入应成功");
    handle.inject_user_message("第二条").expect("注入应成功");
    handle.flush().await.expect("屏障应成功");
    let history = handle.messages().expect("查询应成功");
    assert_eq!(history.len(), 2, "两次注入先于屏障执行");

    handle.clear_messages().expect("清空应成功");
    handle.flush().await.expect("屏障应成功");
    assert!(handle.messages().expect("查询应成功").is_empty());

    handle
        .restore_messages(vec![user_message("恢复的历史")])
        .expect("替换应成功");
    handle
        .prompt("继续", CancellationToken::new())
        .await
        .expect("prompt 应成功");
    assert_eq!(
        provider.context_lens(),
        vec![2],
        "prompt 一定跑在 restore 之后（恢复的历史 + 本轮 user 消息）"
    );
}

/// 系统提示词变更经快照视图可见（flush 屏障后），并随后续请求生效。
#[tokio::test]
async fn system_prompt_change_applies_to_next_request() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (agent, _events) = make_agent(provider.clone(), vec![]);
    let (handle, _task) = agent.spawn();

    assert_eq!(
        handle.system_prompt().expect("查询应成功"),
        "test system prompt",
        "初始提示词来自 builder"
    );
    handle
        .set_system_prompt("重建后的系统提示词".to_string())
        .expect("替换应成功");
    handle.flush().await.expect("屏障应成功");
    assert_eq!(
        handle.system_prompt().expect("查询应成功"),
        "重建后的系统提示词"
    );

    handle
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt 应成功");
    assert_eq!(
        provider.system_prompts(),
        vec![Some("重建后的系统提示词".to_string())],
        "后续请求携带新系统提示词"
    );
}

/// 模型与思考级别变更经快照视图可见（flush 屏障后），并随后续请求生效。
#[tokio::test]
async fn config_changes_visible_via_queries() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (agent, _events) = make_agent(provider.clone(), vec![]);
    let (handle, _task) = agent.spawn();

    handle
        .set_reasoning(Some(ThinkingLevel::High))
        .expect("设置应成功");
    handle.flush().await.expect("屏障应成功");
    assert_eq!(
        handle.reasoning().expect("查询应成功"),
        Some(ThinkingLevel::High)
    );

    let next = Model {
        id: "other-model".to_string(),
        ..model()
    };
    handle.set_model(next).expect("切换应成功");
    handle.flush().await.expect("屏障应成功");
    assert_eq!(handle.model().expect("查询应成功").id, "other-model");

    handle
        .prompt("hi", CancellationToken::new())
        .await
        .expect("prompt 应成功");
    assert_eq!(
        provider.reasonings(),
        vec![Some(ThinkingLevel::High)],
        "后续请求携带新思考级别"
    );
}

/// token 估算查询与 core 的估算口径一致。
#[tokio::test]
async fn context_tokens_matches_estimate() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (agent, _events) = make_agent(provider, vec![]);
    let (handle, _task) = agent.spawn();

    handle
        .prompt("hello", CancellationToken::new())
        .await
        .expect("prompt 应成功");
    let messages = handle.messages().expect("查询应成功");
    let tokens = handle.context_tokens().expect("查询应成功");
    assert_eq!(tokens, estimate_context_tokens(&messages));
}

/// actor 任务 panic：挂起与后续调用一律得到 `ActorError::Gone`，
/// panic 详情经 JoinHandle 暴露。
#[tokio::test]
async fn calls_fail_with_gone_after_actor_panics() {
    struct PanicProvider;

    impl Provider for PanicProvider {
        fn stream(
            &self,
            _model: &Model,
            _context: &Context,
            _options: &StreamOptions,
            _cancel: CancellationToken,
        ) -> nomic_ai::AssistantStream {
            panic!("boom");
        }
    }

    let (agent, _events) = Agent::builder()
        .model(model())
        .provider(Arc::new(PanicProvider))
        .system_prompt("sys")
        .build();
    let (handle, task) = agent.spawn();

    let error = handle
        .prompt("hi", CancellationToken::new())
        .await
        .expect_err("actor panic 后回执 oneshot 被丢弃");
    assert!(matches!(error, ActorError::Gone));

    let error = handle.messages().expect_err("actor 已退出");
    assert!(matches!(error, ActorError::Gone));
    assert!(
        matches!(handle.clear_messages(), Err(ActorError::Gone)),
        "fire-and-forget 同样报告 Gone"
    );

    let join = task.await.expect_err("任务应 panic");
    assert!(join.is_panic());
}

/// 回归（ADR-0035）：run 类命令把整轮 loop 包进一条邮箱命令，运行期间
/// 邮箱不被消费——查询若走邮箱会排队到 run 结束（web `get_state` 超时、
/// 切不回活跃会话的根因）。查询读共享状态视图：run 在 LLM 流上挂起时
/// 即时返回最后一次应用的状态（本轮 user 消息已落史、assistant 未落史）。
#[tokio::test]
async fn queries_during_run_read_latest_applied_state() {
    use nomic_ai::StopReason;
    use tokio::sync::Notify;

    /// 进入流后挂起、直到测试放行的 provider（模拟一轮漫长的 LLM 响应）。
    struct GatedProvider {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl Provider for GatedProvider {
        fn stream(
            &self,
            _model: &Model,
            _context: &Context,
            _options: &StreamOptions,
            _cancel: CancellationToken,
        ) -> nomic_ai::AssistantStream {
            self.entered.notify_one();
            let (tx, stream) = nomic_ai::channel();
            let release = self.release.clone();
            tokio::spawn(async move {
                let _ = tx.send(nomic_ai::AssistantEvent::Start);
                release.notified().await;
                let _ = tx.send(nomic_ai::AssistantEvent::Done {
                    message: Box::new(assistant_message(vec![], StopReason::Stop)),
                });
            });
            stream
        }
    }

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Arc::new(GatedProvider {
        entered: entered.clone(),
        release: release.clone(),
    });
    let (agent, _events) = Agent::builder()
        .model(model())
        .provider(provider)
        .system_prompt("sys")
        .build();
    let (handle, _task) = agent.spawn();

    let run = tokio::spawn({
        let handle = handle.clone();
        async move { handle.prompt("hi", CancellationToken::new()).await }
    });
    // 等 run 挂到 LLM 流上（actor 邮箱此时不再被消费）
    entered.notified().await;

    // 查询不等待 run 结束：同步返回最后一次应用的状态
    let history = handle.messages().expect("运行中查询应即时返回");
    assert_eq!(history.len(), 1, "只有已落史的 user 消息");
    assert!(matches!(history[0], Message::User(_)));
    let _ = handle.model().expect("模型查询不阻塞");
    let _ = handle.reasoning().expect("思考级别查询不阻塞");
    let _ = handle.context_tokens().expect("token 查询不阻塞");
    let _ = handle.stats().expect("统计查询不阻塞");

    // 放行 run，确认最终状态一致
    release.notify_one();
    run.await
        .expect("run 任务应正常结束")
        .expect("prompt 应成功");
    assert_eq!(handle.messages().expect("查询应成功").len(), 2);
}
