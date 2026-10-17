//! S8 独立复核共享支撑：脚本辅助、等待、事件读取与运行中会话播种。
//!
//! 经 `#[path]` 引入多个复核测试二进制；各二进制只使用其中一个子集。

#![allow(
    dead_code,
    reason = "共享支撑模块经 #[path] 引入多个测试二进制，各二进制只使用其中一个子集"
)]
#![allow(
    unreachable_pub,
    reason = "共享测试支撑在测试二进制内以 pub 声明共享项"
)]

use std::path::Path;
use std::time::{Duration, Instant};

use nomic_core::{
    Acceptance, Event, Message, RequestId, Role, SessionId, SessionState, SessionTarget, Step,
    Storage, Submission, ToolCall, ToolCallId, ToolName,
};
use nomic_server::client::Client;

use crate::agent_seam::{self, ProtocolFixture, ScriptedEnv, ScriptedLlm, ScriptedTurn};

pub fn empty_reply() -> ScriptedTurn {
    ScriptedTurn::reply(Vec::<String>::new())
}

pub async fn submit(
    client: &mut Client,
    target: SessionTarget,
    working_dir: &Path,
    content: &str,
) -> Acceptance {
    client
        .submit(&Submission {
            request_id: RequestId::new(),
            target,
            working_dir: working_dir.to_path_buf(),
            content: content.into(),
        })
        .await
        .unwrap()
}

pub async fn submit_new(client: &mut Client, working_dir: &Path, content: &str) -> SessionId {
    match submit(client, SessionTarget::New, working_dir, content).await {
        Acceptance::Accepted { session_id } => session_id,
        Acceptance::Duplicate { .. } => panic!("首次提交不应判重"),
    }
}

pub async fn wait_idle(fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>, session: SessionId) {
    agent_seam::wait_for(|| async {
        fixture
            .storage
            .get_session(session)
            .await
            .unwrap()
            .is_some_and(|stored| stored.state() == SessionState::Idle)
    })
    .await;
}

pub async fn stored_steps(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    session: SessionId,
) -> Vec<Step> {
    fixture.storage.steps(session).await.unwrap()
}

pub async fn stored_user_texts(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    session: SessionId,
) -> Vec<String> {
    stored_steps(fixture, session)
        .await
        .into_iter()
        .filter_map(|step| match step {
            Step::Message(Message::User { text, .. }) => Some(text),
            _ => None,
        })
        .collect()
}

pub fn event_user_texts(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::MessageCompleted { message, .. } if message.role() == Role::User => {
                Some(message.text().to_string())
            }
            _ => None,
        })
        .collect()
}

/// 读事件直到收齐 n 条用户消息（其余事件跳过），返回其文本顺序。
pub async fn collect_user_texts(client: &mut Client, n: usize) -> Vec<String> {
    let mut texts = Vec::with_capacity(n);
    while texts.len() < n {
        if let Event::MessageCompleted { message, .. } = agent_seam::next_event(client).await
            && message.role() == Role::User
        {
            texts.push(message.text().to_string());
        }
    }
    texts
}

/// 非 panic 的轮询等待：超时返回 false，供断言附带诊断信息。
pub async fn settle(timeout: Duration, mut cond: impl AsyncFnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond().await {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 读取 n 条连续的原始事件（含树广播）。
pub async fn read_raw_events(client: &mut Client, n: usize) -> Vec<Event> {
    let mut events = Vec::with_capacity(n);
    for _ in 0..n {
        events.push(agent_seam::next_event(client).await);
    }
    events
}

/// 播种会话并让两个连接都订阅它；返回会话 id。
pub async fn seeded_subscribed(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    a: &mut Client,
    b: &mut Client,
    dir: &Path,
) -> SessionId {
    a.list_sessions().await.unwrap();
    b.list_sessions().await.unwrap();
    let session = submit_new(a, dir, "种子").await;
    wait_idle(fixture, session).await;
    agent_seam::subscribe_synced(a, session).await;
    agent_seam::subscribe_synced(b, session).await;
    // 新建会话的树变更广播发给全部连接：订阅完成后先取走这条广播，
    // 后续断言从会话事件起点开始
    for client in [&mut *a, &mut *b] {
        assert!(matches!(
            agent_seam::next_event(client).await,
            Event::SessionTreeChanged { .. }
        ));
    }
    session
}

pub fn submission(session: SessionId, dir: &Path, content: &str) -> Submission {
    Submission {
        request_id: RequestId::new(),
        target: SessionTarget::Existing(session),
        working_dir: dir.to_path_buf(),
        content: content.into(),
    }
}

pub fn user_texts(steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Message(Message::User { text, .. }) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// 直接落库一个运行中会话现场（不经协议，避免调度器干扰）。
pub async fn seed_running_session(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    dir: &Path,
    steps: Vec<Step>,
) -> SessionId {
    let session = fixture
        .storage
        .create_session(dir.to_path_buf())
        .await
        .unwrap()
        .id();
    for step in steps {
        fixture.storage.append_step(session, step).await.unwrap();
    }
    fixture
        .storage
        .set_state(session, SessionState::Running)
        .await
        .unwrap();
    session
}

pub fn read_call(path: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        tool: ToolName::new("read"),
        arguments: serde_json::json!({"path": path}),
    }
}
