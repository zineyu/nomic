//! 多客户端串行化与广播验收（#42）：两个并发协议客户端交错提交时落库
//! 顺序等于到达顺序、双方收到逐条一致的事件序列、一方断连不影响另一方、
//! 会话树变更广播给全部连接。

#[path = "common/agent_seam.rs"]
mod agent_seam;

use std::path::Path;

use agent_seam::{ProtocolFixture, ScriptedEnv, ScriptedLlm, ScriptedTurn};
use nomic_core::{
    Acceptance, Event, Message, RequestId, Role, SessionId, SessionState, SessionTarget, Step,
    Storage, Submission,
};
use nomic_server::client::Client;

/// 空回复剧本：会话受理一次、请求一次 LLM、落一条空助手消息后静止。
fn empty_reply() -> ScriptedTurn {
    ScriptedTurn::reply(Vec::<String>::new())
}

/// 提交一条消息并返回受理结果。
async fn submit(
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

/// 提交到新会话，返回受理的会话 id。
async fn submit_new(client: &mut Client, working_dir: &Path, content: &str) -> SessionId {
    match submit(client, SessionTarget::New, working_dir, content).await {
        Acceptance::Accepted { session_id } => session_id,
        Acceptance::Duplicate { .. } => panic!("首次提交不应判重"),
    }
}

/// 等待会话回到静止（一次运行完成）。
async fn wait_idle(fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>, session: SessionId) {
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

/// 会话落库的用户消息文本（按步骤顺序）。
async fn stored_user_texts(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    session: SessionId,
) -> Vec<String> {
    fixture
        .storage
        .steps(session)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|step| match step {
            Step::Message(Message::User { text, .. }) => Some(text),
            _ => None,
        })
        .collect()
}

/// 事件序列中的用户消息文本。
fn event_user_texts(events: &[Event]) -> Vec<String> {
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

/// 读取事件直到收齐 n 条用户消息（其余事件跳过），返回其文本顺序。
async fn collect_user_texts(client: &mut Client, n: usize) -> Vec<String> {
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

#[tokio::test]
async fn 交错提交_落库顺序等于到达顺序_且双方事件序列一致() {
    let dir = tempfile::tempdir().unwrap();
    // 每次运行一轮空回复：提交后会话回到静止，隔离出确定的运行边界
    let llm = ScriptedLlm::new(vec![
        empty_reply(),
        empty_reply(),
        empty_reply(),
        empty_reply(),
        empty_reply(),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    // 登记屏障：list 往返证明连接已成为树广播收件人
    a.list_sessions().await.unwrap();
    b.list_sessions().await.unwrap();

    // A 新建会话；此时双方都未订阅，只收到树变更广播
    let session = submit_new(&mut a, dir.path(), "种子").await;
    wait_idle(&fixture, session).await;
    agent_seam::subscribe_synced(&mut a, session).await;
    agent_seam::subscribe_synced(&mut b, session).await;

    // 交错提交：A、B、A、B，每次等会话静止
    for (who, text) in [(0, "一"), (1, "二"), (0, "三"), (1, "四")] {
        let client = if who == 0 { &mut a } else { &mut b };
        let acceptance = submit(client, SessionTarget::Existing(session), dir.path(), text).await;
        assert!(matches!(acceptance, Acceptance::Accepted { .. }));
        wait_idle(&fixture, session).await;
    }

    // 落库用户消息顺序 = 到达顺序
    assert_eq!(
        stored_user_texts(&fixture, session).await,
        ["种子", "一", "二", "三", "四"]
    );

    // 双方收到逐条一致的事件序列：树变更 + 每段（用户消息、助手消息）
    let events_a = agent_seam::read_events(&mut a, 9).await;
    let events_b = agent_seam::read_events(&mut b, 9).await;
    assert_eq!(events_a, events_b, "两个连接必须收到相同事件序列");
    assert!(
        matches!(events_a[0], Event::SessionTreeChanged { .. }),
        "首个事件应为新会话的树变更广播: {:?}",
        events_a[0]
    );
    // 事件中的用户消息顺序 = 落库顺序
    assert_eq!(event_user_texts(&events_a), ["一", "二", "三", "四"]);
}

#[tokio::test]
async fn 同一会话并发提交_双方事件顺序与落库顺序一致() {
    let dir = tempfile::tempdir().unwrap();
    // 并发提交可能落在同一次运行或两次运行；剧本足够覆盖两种情形
    let llm = ScriptedLlm::new(vec![empty_reply(), empty_reply(), empty_reply()]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    a.list_sessions().await.unwrap();
    b.list_sessions().await.unwrap();

    let session = submit_new(&mut a, dir.path(), "种子").await;
    wait_idle(&fixture, session).await;
    agent_seam::subscribe_synced(&mut a, session).await;
    agent_seam::subscribe_synced(&mut b, session).await;

    // 真正的并发提交：不做先后同步
    let (first, second) = tokio::join!(
        submit(&mut a, SessionTarget::Existing(session), dir.path(), "一"),
        submit(&mut b, SessionTarget::Existing(session), dir.path(), "二"),
    );
    assert!(matches!(first, Acceptance::Accepted { .. }));
    assert!(matches!(second, Acceptance::Accepted { .. }));
    // 两条用户消息都已落库
    agent_seam::wait_for(|| async { stored_user_texts(&fixture, session).await.len() == 3 }).await;

    // 到达顺序的唯一事实源是落库顺序；两个连接观察到的用户消息顺序必须
    // 逐条等于它
    let stored = stored_user_texts(&fixture, session).await;
    let expected = stored[1..].to_vec();
    assert_eq!(expected.len(), 2);
    assert_eq!(collect_user_texts(&mut a, 2).await, expected);
    assert_eq!(collect_user_texts(&mut b, 2).await, expected);
}

#[tokio::test]
async fn 运行中受理_回合边界后落库并双向广播一致() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![
        empty_reply(),
        ScriptedTurn::gated_reply(gate.clone(), ["甲"]),
        empty_reply(),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    a.list_sessions().await.unwrap();
    b.list_sessions().await.unwrap();

    let session = submit_new(&mut a, dir.path(), "种子").await;
    wait_idle(&fixture, session).await;
    agent_seam::subscribe_synced(&mut a, session).await;
    agent_seam::subscribe_synced(&mut b, session).await;

    // A 提交“一”触发运行，被闸门钉在首个增量之前
    assert!(matches!(
        submit(&mut a, SessionTarget::Existing(session), dir.path(), "一").await,
        Acceptance::Accepted { .. }
    ));
    agent_seam::wait_for(|| async { fixture.llm.captured().len() == 2 }).await;

    // B 在运行中提交“二”：受理成功，但回合边界前不进入步骤日志
    assert!(matches!(
        submit(&mut b, SessionTarget::Existing(session), dir.path(), "二").await,
        Acceptance::Accepted { .. }
    ));
    assert_eq!(stored_user_texts(&fixture, session).await, ["种子", "一"]);

    gate.release_all(2);
    wait_idle(&fixture, session).await;

    // 回合边界后“二”排在本回合助手消息“甲”之后，并驱动下一回合
    let steps = fixture.storage.steps(session).await.unwrap();
    assert!(
        matches!(&steps[3], Step::Message(Message::Assistant { text, .. }) if text == "甲")
            && matches!(&steps[4], Step::Message(Message::User { text, .. }) if text == "二"),
        "新用户消息必须排在上一回合助手消息之后: {steps:?}"
    );
    assert_eq!(
        stored_user_texts(&fixture, session).await,
        ["种子", "一", "二"]
    );

    // 双方收到相同的完整序列：树广播 + 用户“一” + 甲增量/完成 + 用户“二” + 下一回合完成
    let events_a = agent_seam::read_events(&mut a, 6).await;
    let events_b = agent_seam::read_events(&mut b, 6).await;
    assert_eq!(events_a, events_b, "运行中受理后双方事件序列必须一致");
    assert_eq!(event_user_texts(&events_a), ["一", "二"]);
    assert!(
        matches!(&events_a[4], Event::MessageCompleted { message, .. } if message.text() == "二"),
        "用户“二”必须在助手“甲”之后广播: {events_a:?}"
    );
}

#[tokio::test]
async fn 并发提交不同会话_双方广播序列一致() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![
        empty_reply(),
        empty_reply(),
        ScriptedTurn::reply(["好"]),
        ScriptedTurn::reply(["好"]),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    a.list_sessions().await.unwrap();
    b.list_sessions().await.unwrap();

    let s1 = submit_new(&mut a, dir.path(), "甲").await;
    wait_idle(&fixture, s1).await;
    let s2 = submit_new(&mut b, dir.path(), "乙").await;
    wait_idle(&fixture, s2).await;

    // 两个客户端都订阅两个会话
    for session in [s1, s2] {
        agent_seam::subscribe_synced(&mut a, session).await;
        agent_seam::subscribe_synced(&mut b, session).await;
    }

    // 并发提交：A → S1，B → S2，不做额外同步
    let (first, second) = tokio::join!(
        submit(&mut a, SessionTarget::Existing(s1), dir.path(), "一"),
        submit(&mut b, SessionTarget::Existing(s2), dir.path(), "二"),
    );
    assert!(matches!(first, Acceptance::Accepted { .. }));
    assert!(matches!(second, Acceptance::Accepted { .. }));
    wait_idle(&fixture, s1).await;
    wait_idle(&fixture, s2).await;

    // 两个树变更 + 两条用户消息 + 两条助手消息（各含增量与完成）
    let events_a = agent_seam::read_events(&mut a, 8).await;
    let events_b = agent_seam::read_events(&mut b, 8).await;
    assert_eq!(events_a, events_b, "并发广播下两连接事件序列必须一致");
    let mut users = event_user_texts(&events_a);
    users.sort();
    assert_eq!(users, ["一", "二"]);
    let assistants = events_a
        .iter()
        .filter(|event| matches!(event, Event::MessageCompleted { message, .. } if message.role() == Role::Assistant))
        .count();
    assert_eq!(assistants, 2, "两个会话各一条助手消息: {events_a:?}");
}

#[tokio::test]
async fn 一方断开不影响另一方接收与输入() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![empty_reply(), ScriptedTurn::reply(["回复"])]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    a.list_sessions().await.unwrap();
    b.list_sessions().await.unwrap();

    let session = submit_new(&mut a, dir.path(), "种子").await;
    wait_idle(&fixture, session).await;
    agent_seam::subscribe_synced(&mut a, session).await;
    agent_seam::subscribe_synced(&mut b, session).await;

    // A 断开：其订阅与发送端被摘除，B 不受影响
    drop(a);
    let acceptance = submit(&mut b, SessionTarget::Existing(session), dir.path(), "继续").await;
    assert!(matches!(acceptance, Acceptance::Accepted { .. }));
    wait_idle(&fixture, session).await;

    // B 仍收到自己提交的用户消息与助手回复（首个为种子会话的树广播）
    let events = agent_seam::read_events(&mut b, 4).await;
    assert!(
        matches!(events[0], Event::SessionTreeChanged { .. }),
        "首个事件应为新会话的树变更广播: {:?}",
        events[0]
    );
    assert_eq!(event_user_texts(&events), ["继续"]);
    assert!(
        matches!(&events[3], Event::MessageCompleted { message, .. } if message.text() == "回复"),
        "断开的一方不得影响另一方的回复: {:?}",
        events[3]
    );
}

#[tokio::test]
async fn 树变更广播给全部连接_无需订阅() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![empty_reply()]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    a.list_sessions().await.unwrap();
    b.list_sessions().await.unwrap();

    // A 新建会话：B 未订阅任何会话，仍收到树变更广播
    let session = submit_new(&mut a, dir.path(), "种子").await;
    wait_idle(&fixture, session).await;
    let created = agent_seam::read_events(&mut b, 1).await;
    match &created[0] {
        Event::SessionTreeChanged { tree } => {
            assert_eq!(tree.sessions().len(), 1);
            assert_eq!(tree.sessions()[0].id(), session);
        }
        other => panic!("期望树变更广播: {other:?}"),
    }

    // A 分叉：B 收到含父子两会话的树变更广播
    let steps = fixture.storage.steps(session).await.unwrap();
    let fork_point = match &steps[0] {
        Step::Message(message) => message.id(),
        other => panic!("首步骤必须是消息: {other:?}"),
    };
    let tree = a.fork(session, fork_point).await.unwrap();
    assert_eq!(tree.sessions().len(), 2);
    let forked = agent_seam::read_events(&mut b, 1).await;
    match &forked[0] {
        Event::SessionTreeChanged { tree } => assert_eq!(tree.sessions().len(), 2),
        other => panic!("期望分叉后的树变更广播: {other:?}"),
    }
}
