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
use support::{MockProvider, collect_events, make_agent, model, text_done};
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

    let history = handle.messages().await.expect("查询应成功");
    assert_eq!(history.len(), 2);
    assert_eq!(provider.context_lens(), vec![1], "上下文含注入的 user 消息");

    // 全部句柄断开后 actor 任务自然退出
    drop(handle);
    task.await.expect("actor 应正常退出");
}

/// fire-and-forget 变更按邮箱 FIFO 生效：紧随的查询/ prompt 一定看到结果。
#[tokio::test]
async fn mutations_apply_in_mailbox_order() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (agent, _events) = make_agent(provider.clone(), vec![]);
    let (handle, _task) = agent.spawn();

    handle.inject_user_message("第一条").expect("注入应成功");
    handle.inject_user_message("第二条").expect("注入应成功");
    let history = handle.messages().await.expect("查询应成功");
    assert_eq!(history.len(), 2, "两次注入先于查询执行");

    handle.clear_messages().expect("清空应成功");
    assert!(handle.messages().await.expect("查询应成功").is_empty());

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

/// 模型与思考级别变更经查询命令可见，并随后续请求生效。
#[tokio::test]
async fn config_changes_visible_via_queries() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let (agent, _events) = make_agent(provider.clone(), vec![]);
    let (handle, _task) = agent.spawn();

    handle
        .set_reasoning(Some(ThinkingLevel::High))
        .expect("设置应成功");
    assert_eq!(
        handle.reasoning().await.expect("查询应成功"),
        Some(ThinkingLevel::High)
    );

    let next = Model {
        id: "other-model".to_string(),
        ..model()
    };
    handle.set_model(next).expect("切换应成功");
    assert_eq!(handle.model().await.expect("查询应成功").id, "other-model");

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
    let messages = handle.messages().await.expect("查询应成功");
    let tokens = handle.context_tokens().await.expect("查询应成功");
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

    let error = handle.messages().await.expect_err("actor 已退出");
    assert!(matches!(error, ActorError::Gone));
    assert!(
        matches!(handle.clear_messages(), Err(ActorError::Gone)),
        "fire-and-forget 同样报告 Gone"
    );

    let join = task.await.expect_err("任务应 panic");
    assert!(join.is_panic());
}
