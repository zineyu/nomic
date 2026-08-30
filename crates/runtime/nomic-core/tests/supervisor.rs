//! AgentSupervisor 集成测试：并行 agent 生命周期、非阻塞消息发送、等待结果、资源回收。

#[allow(dead_code)]
#[path = "agent_loop/support.rs"]
mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use nomic_ai::{ApiKind, Message, Model};
use nomic_core::{
    AgentSupervisor, CreateAgentRequest, SupervisorConfig, SupervisorError, shared_model,
};
use support::{MockProvider, model, text_done};
use tokio_util::sync::CancellationToken;

/// 创建 supervisor（使用 mock provider）。
fn supervisor(max_agents: usize) -> Arc<AgentSupervisor> {
    let provider = MockProvider::new(vec![]);
    Arc::new(AgentSupervisor::new(
        provider,
        vec![model()],
        BTreeMap::new(),
        shared_model(model()),
        SupervisorConfig { max_agents },
    ))
}

/// 指定 provider / id 的模型（别名与解析测试用）。
fn named_model(provider: &str, id: &str, reasoning: bool, vision: bool) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: ApiKind::OpenAiCompletions,
        provider: provider.to_string(),
        base_url: "http://localhost".to_string(),
        reasoning,
        vision,
        context_window: 128_000,
        max_tokens: 4096,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    }
}

/// 创建一个子 agent 并返回其 ID。
async fn create_child(supervisor: &AgentSupervisor) -> nomic_core::AgentId {
    supervisor
        .create(CreateAgentRequest {
            id: None,
            system_prompt: "test".to_string(),
            tools: vec![],
            model: model(),
            provider: None,
            stream_options: None,
        })
        .await
        .expect("create 应成功")
}

/// 创建一个带自定义 provider 的子 agent。
async fn create_child_with_provider(
    supervisor: &AgentSupervisor,
    provider: Arc<dyn nomic_ai::Provider>,
) -> nomic_core::AgentId {
    supervisor
        .create(CreateAgentRequest {
            id: None,
            system_prompt: "test".to_string(),
            tools: vec![],
            model: model(),
            provider: Some(provider),
            stream_options: None,
        })
        .await
        .expect("create 应成功")
}

// ── 创建与关闭 ─────────────────────────────────────────────────────────

#[tokio::test]
async fn create_returns_unique_ids() {
    let sup = supervisor(8);
    let a = create_child(&sup).await;
    let b = create_child(&sup).await;
    assert_ne!(a, b, "不同 agent 应有不同 ID");
    assert_eq!(sup.count().await, 2);
}

#[tokio::test]
async fn create_with_custom_id() {
    let sup = supervisor(8);
    let id = sup
        .create(CreateAgentRequest {
            id: Some("my-agent".to_string()),
            system_prompt: "test".to_string(),
            tools: vec![],
            model: model(),
            provider: None,
            stream_options: None,
        })
        .await
        .expect("create 应成功");
    assert_eq!(id.to_string(), "my-agent");
}

#[tokio::test]
async fn create_respects_max_agents_limit() {
    let sup = supervisor(2);
    create_child(&sup).await;
    create_child(&sup).await;
    let err = sup
        .create(CreateAgentRequest {
            id: None,
            system_prompt: "test".to_string(),
            tools: vec![],
            model: model(),
            provider: None,
            stream_options: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::MaxAgentsReached(2)));
}

#[tokio::test]
async fn close_removes_agent() {
    let sup = supervisor(8);
    let id = create_child(&sup).await;
    assert_eq!(sup.count().await, 1);
    sup.close(&id).await.expect("close 应成功");
    assert_eq!(sup.count().await, 0);
}

#[tokio::test]
async fn close_nonexistent_returns_not_found() {
    let sup = supervisor(8);
    let err = sup
        .close(&nomic_core::AgentId("nope".to_string()))
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::NotFound(_)));
}

#[tokio::test]
async fn close_all_removes_everything() {
    let sup = supervisor(8);
    create_child(&sup).await;
    create_child(&sup).await;
    create_child(&sup).await;
    assert_eq!(sup.count().await, 3);
    sup.close_all().await;
    assert_eq!(sup.count().await, 0);
}

// ── 非阻塞 send_message + wait_result ─────────────────────────────────

#[tokio::test]
async fn send_message_and_wait_result_roundtrip() {
    let provider = MockProvider::new(vec![text_done("hello from child")]);
    let sup = supervisor(8);
    let id = create_child_with_provider(&sup, provider).await;

    // send_message 非阻塞，立即返回
    sup.send_message(&id, "hi", CancellationToken::new())
        .await
        .expect("send 应成功");

    // wait_result 阻塞等待完成
    let messages = sup.wait_result(&id).await.expect("wait 应成功");
    // handle.prompt() 返回 user + assistant 两条消息
    assert!(!messages.is_empty(), "应返回至少 assistant 消息");
    let last = messages.last().expect("应有消息");
    if let Message::Assistant(assistant) = last {
        assert!(!assistant.content.is_empty());
    } else {
        panic!("最后一条应是 assistant 消息");
    }
}

#[tokio::test]
async fn send_message_when_already_running_returns_error() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let sup = supervisor(8);
    let id = create_child_with_provider(&sup, provider).await;

    sup.send_message(&id, "first", CancellationToken::new())
        .await
        .expect("第一次 send 应成功");

    let err = sup
        .send_message(&id, "second", CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisorError::AlreadyRunning(_)));
}

#[tokio::test]
async fn wait_result_when_not_running_returns_error() {
    let sup = supervisor(8);
    let id = create_child(&sup).await;

    let err = sup.wait_result(&id).await.unwrap_err();
    assert!(matches!(err, SupervisorError::NotRunning(_)));
}

// ── 并行 fork-join ─────────────────────────────────────────────────────

#[tokio::test]
async fn fork_join_parallel_execution() {
    // 两个 agent 各自有一段脚本
    let provider_a = MockProvider::new(vec![text_done("response A")]);
    let provider_b = MockProvider::new(vec![text_done("response B")]);
    let sup = supervisor(8);

    let id_a = create_child_with_provider(&sup, provider_a).await;
    let id_b = create_child_with_provider(&sup, provider_b).await;

    // 并行发送（非阻塞）
    sup.send_message(&id_a, "task A", CancellationToken::new())
        .await
        .expect("send A 应成功");
    sup.send_message(&id_b, "task B", CancellationToken::new())
        .await
        .expect("send B 应成功");

    // wait_all 并发等待
    let results = sup
        .wait_all(&[id_a.clone(), id_b.clone()])
        .await
        .expect("wait_all 应成功");

    assert_eq!(results.len(), 2);
    assert!(results.contains_key(&id_a));
    assert!(results.contains_key(&id_b));
}

#[tokio::test]
async fn wait_all_partial_failure() {
    // 一个 agent 正常，另一个没有 pending task
    let provider = MockProvider::new(vec![text_done("ok")]);
    let sup = supervisor(8);

    let id_a = create_child_with_provider(&sup, provider).await;
    let id_b = create_child(&sup).await;

    sup.send_message(&id_a, "task", CancellationToken::new())
        .await
        .expect("send 应成功");
    // id_b 没有 send_message

    let err = sup.wait_all(&[id_a, id_b]).await.unwrap_err();
    assert!(matches!(err, SupervisorError::NotRunning(_)));
}

// ── 状态查询 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn list_agents_shows_status() {
    let sup = supervisor(8);
    let id = create_child(&sup).await;

    let statuses = sup.list().await;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].id, id);
    assert!(!statuses[0].is_running);
    assert_eq!(statuses[0].model_id, "mock-model");
}

#[tokio::test]
async fn status_reflects_running_state() {
    let provider = MockProvider::new(vec![text_done("ok")]);
    let sup = supervisor(8);
    let id = create_child_with_provider(&sup, provider).await;

    // 空闲时 is_running = false
    let status = sup.status(&id).await.expect("status 应成功");
    assert!(!status.is_running);

    // send_message 后 is_running = true
    sup.send_message(&id, "go", CancellationToken::new())
        .await
        .expect("send 应成功");
    let status = sup.status(&id).await.expect("status 应成功");
    assert!(status.is_running);

    // wait_result 后 is_running = false
    sup.wait_result(&id).await.expect("wait 应成功");
    let status = sup.status(&id).await.expect("status 应成功");
    assert!(!status.is_running);
}

// ── 可用模型 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn available_models_returns_configured_list() {
    let sup = supervisor(8);
    let models = sup.available_models();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "mock-model");
}

// ── 模型解析：别名 / 全形式 / 裸 id / 继承 ─────────────────────────────

/// 装配含别名与多 provider 模型的 supervisor（解析测试共用）。
fn supervisor_with_models() -> Arc<AgentSupervisor> {
    let smart = named_model("anthropic", "claude-opus-4", true, true);
    let fast_a = named_model("openai", "gpt-4o-mini", false, false);
    let fast_b = named_model("azure", "gpt-4o-mini", false, false);
    let aliases = BTreeMap::from([("smart".to_string(), smart.clone())]);
    Arc::new(AgentSupervisor::new(
        MockProvider::new(vec![]),
        vec![smart, fast_a, fast_b],
        aliases,
        shared_model(model()),
        SupervisorConfig { max_agents: 8 },
    ))
}

#[tokio::test]
async fn resolve_model_prefers_alias_then_qualified_then_bare_id() {
    let sup = supervisor_with_models();
    // 别名命中
    let model = sup.resolve_model("smart").expect("alias 应命中");
    assert_eq!(model.id, "claude-opus-4");
    // <provider>/<id> 全形式
    let model = sup
        .resolve_model("openai/gpt-4o-mini")
        .expect("全形式应命中");
    assert_eq!(model.provider, "openai");
    // 裸 id 歧义（两个 provider 同名模型）
    let error = sup
        .resolve_model("gpt-4o-mini")
        .expect_err("裸 id 歧义应报错");
    let message = error.to_string();
    assert!(message.contains("ambiguous"), "{message}");
    assert!(message.contains("openai/gpt-4o-mini"), "{message}");
    assert!(message.contains("azure/gpt-4o-mini"), "{message}");
}

#[tokio::test]
async fn resolve_model_unknown_lists_aliases_and_models() {
    let sup = supervisor_with_models();
    let error = sup.resolve_model("no-such").expect_err("未知标识应报错");
    let message = error.to_string();
    assert!(
        message.contains("unknown model or alias \"no-such\""),
        "{message}"
    );
    assert!(message.contains("smart"), "报错应列出别名：{message}");
    assert!(message.contains("anthropic/claude-opus-4"), "{message}");
}

#[tokio::test]
async fn inherited_model_follows_shared_cell_updates() {
    let cell = shared_model(model());
    let sup = Arc::new(AgentSupervisor::new(
        MockProvider::new(vec![]),
        vec![model()],
        BTreeMap::new(),
        cell.clone(),
        SupervisorConfig { max_agents: 8 },
    ));
    assert_eq!(sup.inherited_model().id, "mock-model");
    // 入口在主 agent 模型切换时写入共享单元，继承随之更新
    *cell.write().expect("lock") = named_model("openai", "gpt-4o", false, true);
    assert_eq!(sup.inherited_model().id, "gpt-4o");
    assert!(sup.inherited_model().vision);
}
