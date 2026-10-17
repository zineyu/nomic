//! 独立复核（S8 #42）：多客户端串行化与广播（在线路径 + 存储原语）。
//!
//! 单独推导验收 1/2/3：落库顺序、双方事件序列逐条一致（含增量）、
//! 断连隔离，以及分派任务与调度任务交错时的广播顺序。

#[path = "common/agent_seam.rs"]
mod agent_seam;

#[path = "common/s8_support.rs"]
mod s8_support;

use std::time::Duration;

use agent_seam::{ProtocolFixture, ScriptedEnv, ScriptedLlm, ScriptedTurn, WAIT};
use nomic_core::{Acceptance, Event, Message, Role, SessionState, SessionTarget, Step, Storage};
use nomic_server::storage::SQLiteStorage;
use rusqlite::Connection;
use s8_support::{
    collect_user_texts, empty_reply, event_user_texts, read_raw_events, seeded_subscribed, settle,
    stored_steps, stored_user_texts, submission, submit, submit_new, user_texts, wait_idle,
};

// ---------------------------------------------------------------------------
// 探测 1：运行中受理的第二条消息必须被执行（存储是待处理工作的事实源）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 运行中受理的第二条消息不得静默丢弃() {
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
    let session = seeded_subscribed(&fixture, &mut a, &mut b, dir.path()).await;

    // 第一条进入运行并被闸门钉在首个增量之前
    assert!(matches!(
        submit(&mut a, SessionTarget::Existing(session), dir.path(), "一").await,
        Acceptance::Accepted { .. }
    ));
    assert!(
        settle(WAIT, || async { fixture.llm.captured().len() == 2 }).await,
        "第一条消息的运行未开始"
    );

    // 运行中受理第二条：回执 Accepted，立即持久化为待处理提交
    assert!(matches!(
        submit(&mut b, SessionTarget::Existing(session), dir.path(), "二").await,
        Acceptance::Accepted { .. }
    ));
    // 回合进行中：消息尚未进入步骤日志，等回合边界再排入
    assert_eq!(stored_user_texts(&fixture, session).await, ["种子", "一"]);

    gate.release_all(2);
    wait_idle(&fixture, session).await;

    // 第二条已在回合边界排入并被执行：助手消息“甲”在前，新用户消息“二”在后
    let steps = stored_steps(&fixture, session).await;
    assert_eq!(
        stored_user_texts(&fixture, session).await,
        ["种子", "一", "二"],
        "运行中受理的第二条消息不得被静默丢弃：{steps:?}"
    );
    assert!(
        matches!(&steps[3], Step::Message(Message::Assistant { text, .. }) if text == "甲")
            && matches!(&steps[4], Step::Message(Message::User { text, .. }) if text == "二"),
        "新用户消息必须排在上一回合助手消息之后: {steps:?}"
    );
    // 第二条已落入步骤日志，必须再驱动一次运行把它执行掉
    assert!(
        settle(Duration::from_secs(2), || async {
            fixture.llm.captured().len() == 3
        })
        .await,
        "运行中受理的第二条消息被静默丢弃：captured={}，步={:?}",
        fixture.llm.captured().len(),
        stored_steps(&fixture, session).await
    );
}

// ---------------------------------------------------------------------------
// 探测 2：分派任务（用户消息/树）与调度任务（会话事件）交错时序列一致
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 跨任务交错_两连接事件序列逐条一致且含增量顺序() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![
        empty_reply(),
        ScriptedTurn::gated_reply(gate.clone(), ["甲", "乙"]),
        empty_reply(),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    let session = seeded_subscribed(&fixture, &mut a, &mut b, dir.path()).await;

    assert!(matches!(
        submit(&mut a, SessionTarget::Existing(session), dir.path(), "一").await,
        Acceptance::Accepted { .. }
    ));
    assert!(settle(WAIT, || async { fixture.llm.captured().len() == 2 }).await);

    // 运行中：分叉与第二条用户消息（均由 dispatcher 产生）
    let steps = stored_steps(&fixture, session).await;
    let fork_point = match &steps[0] {
        Step::Message(message) => message.id(),
        other => panic!("首步骤必须是消息: {other:?}"),
    };
    let tree = a.fork(session, fork_point).await.unwrap();
    assert_eq!(tree.sessions().len(), 2);
    assert!(matches!(
        submit(&mut b, SessionTarget::Existing(session), dir.path(), "二").await,
        Acceptance::Accepted { .. }
    ));

    // 第二条在回合进行中，先持久化为待处理提交：此处只应到用户“一”与分叉树广播
    let head_a = read_raw_events(&mut a, 2).await;
    let head_b = read_raw_events(&mut b, 2).await;
    assert_eq!(head_a, head_b, "分派与调度交错时两连接头部序列必须一致");
    assert_eq!(event_user_texts(&head_a[..1]), ["一"]);
    assert!(
        matches!(head_a[1], Event::SessionTreeChanged { .. }),
        "第二条事件应为分叉树广播: {:?}",
        head_a[1]
    );

    gate.release_all(3);
    // 尾部：两段增量 + 本回合完成，随后才是回合边界排入的用户“二”与下一回合完成
    let tail_a = read_raw_events(&mut a, 5).await;
    let tail_b = read_raw_events(&mut b, 5).await;
    assert_eq!(tail_a, tail_b, "增量与回合边界顺序必须逐条一致");
    match &tail_a[..] {
        [
            Event::TokenDelta { delta: first, .. },
            Event::TokenDelta { delta: second, .. },
            Event::MessageCompleted { message, .. },
            Event::MessageCompleted {
                message: deferred, ..
            },
            Event::MessageCompleted { message: next, .. },
        ] => {
            assert_eq!(first, "甲");
            assert_eq!(second, "乙");
            assert_eq!(message.role(), Role::Assistant);
            assert_eq!(message.text(), "甲乙");
            assert_eq!(deferred.text(), "二", "新用户消息必须在助手完成之后广播");
            assert_eq!(next.role(), Role::Assistant);
        }
        other => panic!("尾部事件序列不符: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 探测 3：一方运行中断开不影响另一方接收与输入
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 运行中断开一方_另一方继续接收并保持输入能力() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![
        empty_reply(),
        ScriptedTurn::gated_reply(gate.clone(), ["甲", "乙"]),
        empty_reply(),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    let session = seeded_subscribed(&fixture, &mut a, &mut b, dir.path()).await;

    assert!(matches!(
        submit(&mut a, SessionTarget::Existing(session), dir.path(), "一").await,
        Acceptance::Accepted { .. }
    ));
    assert!(settle(WAIT, || async { fixture.llm.captured().len() == 2 }).await);

    // 运行中 A 断开；B 必须继续收到全部会话事件（用户消息 + 两段增量 + 完成）
    drop(a);
    gate.release_all(3);
    let events = read_raw_events(&mut b, 4).await;
    assert_eq!(event_user_texts(&events[..1]), ["一"]);
    match &events[1..] {
        [
            Event::TokenDelta { delta: first, .. },
            Event::TokenDelta { delta: second, .. },
            Event::MessageCompleted { message, .. },
        ] => {
            assert_eq!(first, "甲");
            assert_eq!(second, "乙");
            assert_eq!(message.text(), "甲乙");
        }
        other => panic!("断开方不得影响另一方的事件流: {other:?}"),
    }

    // B 的输入仍被受理并执行
    wait_idle(&fixture, session).await;
    assert!(matches!(
        submit(&mut b, SessionTarget::Existing(session), dir.path(), "二").await,
        Acceptance::Accepted { .. }
    ));
    assert!(settle(WAIT, || async { fixture.llm.captured().len() == 3 }).await);
    let next = read_raw_events(&mut b, 2).await;
    assert_eq!(event_user_texts(&next[..1]), ["二"]);
    assert!(matches!(
        &next[1],
        Event::MessageCompleted { message, .. } if message.role() == Role::Assistant
    ));
    assert_eq!(
        stored_user_texts(&fixture, session).await,
        ["种子", "一", "二"]
    );
}

// ---------------------------------------------------------------------------
// 探测 4：并发提交时两连接的用户事件顺序逐条等于落库顺序
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 并发提交_两连接用户事件顺序逐条等于落库顺序() {
    let dir = tempfile::tempdir().unwrap();
    let turns: Vec<ScriptedTurn> = (0..16).map(|_| empty_reply()).collect();
    let fixture =
        ProtocolFixture::start(dir.path(), ScriptedLlm::new(turns), ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    let session = seeded_subscribed(&fixture, &mut a, &mut b, dir.path()).await;

    let run_a = async {
        for i in 0..6 {
            let text = format!("a{i}");
            assert!(matches!(
                submit(&mut a, SessionTarget::Existing(session), dir.path(), &text).await,
                Acceptance::Accepted { .. }
            ));
        }
    };
    let run_b = async {
        for i in 0..6 {
            let text = format!("b{i}");
            assert!(matches!(
                submit(&mut b, SessionTarget::Existing(session), dir.path(), &text).await,
                Acceptance::Accepted { .. }
            ));
        }
    };
    tokio::join!(run_a, run_b);

    agent_seam::wait_for(|| async { stored_user_texts(&fixture, session).await.len() == 13 }).await;
    let stored = stored_user_texts(&fixture, session).await;
    let expected = stored[1..].to_vec();
    assert_eq!(expected.len(), 12, "并发提交全部落库: {stored:?}");
    assert_eq!(collect_user_texts(&mut a, 12).await, expected);
    assert_eq!(collect_user_texts(&mut b, 12).await, expected);
}

// ---------------------------------------------------------------------------
// 探测 5：用另一连接的观测钉住到达顺序，再核对落库顺序
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 观测钉住到达顺序后_落库顺序与两连接一致() {
    let dir = tempfile::tempdir().unwrap();
    let turns: Vec<ScriptedTurn> = (0..5).map(|_| empty_reply()).collect();
    let fixture =
        ProtocolFixture::start(dir.path(), ScriptedLlm::new(turns), ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    let session = seeded_subscribed(&fixture, &mut a, &mut b, dir.path()).await;

    let texts = ["一", "二", "三", "四"];
    for (index, text) in texts.iter().enumerate() {
        // 提交后先让两个连接都读到这条用户事件，再发下一条：到达顺序被钉死
        if index % 2 == 0 {
            assert!(matches!(
                submit(&mut a, SessionTarget::Existing(session), dir.path(), text).await,
                Acceptance::Accepted { .. }
            ));
            assert_eq!(collect_user_texts(&mut b, 1).await, [*text]);
            assert_eq!(collect_user_texts(&mut a, 1).await, [*text]);
        } else {
            assert!(matches!(
                submit(&mut b, SessionTarget::Existing(session), dir.path(), text).await,
                Acceptance::Accepted { .. }
            ));
            assert_eq!(collect_user_texts(&mut a, 1).await, [*text]);
            assert_eq!(collect_user_texts(&mut b, 1).await, [*text]);
        }
    }
    assert_eq!(
        stored_user_texts(&fixture, session).await,
        ["种子", "一", "二", "三", "四"]
    );
}

// ---------------------------------------------------------------------------
// 探测 6：列表响应与树广播是不同帧，广播事件不被响应读取吞掉
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 树广播先到时_列表响应仍可返回且广播事件保留() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![empty_reply()]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    a.list_sessions().await.unwrap();
    b.list_sessions().await.unwrap();

    let session = submit_new(&mut a, dir.path(), "种子").await;
    wait_idle(&fixture, session).await;

    let steps = stored_steps(&fixture, session).await;
    let fork_point = match &steps[0] {
        Step::Message(message) => message.id(),
        other => panic!("首步骤必须是消息: {other:?}"),
    };
    let forked = b.fork(session, fork_point).await.unwrap();
    assert_eq!(forked.sessions().len(), 2);

    // A 的流里已有树广播；列表请求必须跳过它并返回快照
    let listed = a.list_sessions().await.unwrap();
    assert_eq!(listed.sessions().len(), 2);
    let event = agent_seam::next_event(&mut a).await;
    assert!(
        matches!(event, Event::SessionTreeChanged { .. }),
        "树广播事件被响应读取吞掉: {event:?}"
    );
}

// ---------------------------------------------------------------------------
// 探测 8：回放期间其他会话的实时事件必须保留并在回放后按序送达
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 回放期间其他会话实时事件回放后仍按序送达() {
    let dir = tempfile::tempdir().unwrap();
    let turns = vec![empty_reply(), empty_reply(), ScriptedTurn::reply(["答复"])];
    let fixture =
        ProtocolFixture::start(dir.path(), ScriptedLlm::new(turns), ScriptedEnv::inert()).await;
    let mut a = fixture.connect().await;
    a.list_sessions().await.unwrap();

    let first = submit_new(&mut a, dir.path(), "甲").await;
    wait_idle(&fixture, first).await;
    let second = submit_new(&mut a, dir.path(), "乙").await;
    wait_idle(&fixture, second).await;
    agent_seam::subscribe_synced(&mut a, second).await;

    // 第二个会话实时产生用户消息与助手消息，帧到达后尚未被客户端读取
    assert!(matches!(
        submit(&mut a, SessionTarget::Existing(second), dir.path(), "丙").await,
        Acceptance::Accepted { .. }
    ));
    wait_idle(&fixture, second).await;
    assert_eq!(fixture.llm.captured().len(), 3);

    // 回放第一个会话：回放边界只含第一个会话历史，第二个会话实时事件入缓冲
    let replayed = a.resume(first).await.unwrap();
    let texts: Vec<String> = replayed.iter().map(|m| m.text().to_string()).collect();
    assert_eq!(texts, ["甲", ""], "回放只含第一个会话的历史: {texts:?}");

    // 缓冲事件在回放后仍按到达顺序送达，不丢失（跳过树变更广播）
    let mut users = Vec::new();
    let mut assistants = Vec::new();
    for _ in 0..6 {
        match agent_seam::next_session_event(&mut a).await {
            Event::MessageCompleted { message, .. } if message.role() == Role::User => {
                users.push(message.text().to_string());
            }
            Event::MessageCompleted { message, .. } if message.role() == Role::Assistant => {
                assistants.push(message.text().to_string());
                break;
            }
            _ => {}
        }
    }
    assert_eq!(users, ["丙"], "回放后必须送达回放期间的实时用户消息");
    assert_eq!(assistants, ["答复"], "回放后必须送达回放期间的实时助手消息");
}

// ---------------------------------------------------------------------------
// 探测 9：延迟落库原语——去重、排入、收尾状态机（存储层直驱）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 待处理提交去重排入与收尾状态机() {
    let dir = tempfile::tempdir().unwrap();
    let storage = SQLiteStorage::open(dir.path().join("nomic.db")).unwrap();
    let session = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap()
        .id();

    // 静止会话：立即排入，随返回消息广播
    let first = storage
        .record_submission(&submission(session, dir.path(), "一"))
        .await
        .unwrap();
    assert!(matches!(first.acceptance, Acceptance::Accepted { .. }));
    assert!(first.message.is_some(), "静止受理必须返回用户消息");
    assert_eq!(
        storage.get_session(session).await.unwrap().unwrap().state(),
        SessionState::Pending
    );
    assert!(storage.claim_session(session).await.unwrap());

    // 运行中受理：不排入步骤，消息持久化为待处理，状态保持运行中
    let deferred = submission(session, dir.path(), "二");
    let second = storage.record_submission(&deferred).await.unwrap();
    assert!(matches!(second.acceptance, Acceptance::Accepted { .. }));
    assert!(second.message.is_none(), "运行中受理不得立即排入");
    assert_eq!(
        storage.get_session(session).await.unwrap().unwrap().state(),
        SessionState::Running,
        "延迟受理不得改变运行中状态"
    );
    assert_eq!(user_texts(&storage.steps(session).await.unwrap()), ["一"]);

    // 同一 requestId 在延迟期间重复提交：判重且不重复排队
    let duplicate = storage.record_submission(&deferred).await.unwrap();
    assert!(matches!(duplicate.acceptance, Acceptance::Duplicate { .. }));
    assert!(duplicate.message.is_none());

    // 有待处理提交：收尾保持运行中
    assert!(!storage.quiesce_session(session).await.unwrap());
    assert_eq!(
        storage.get_session(session).await.unwrap().unwrap().state(),
        SessionState::Running
    );

    // 排入恰好一条去重后的消息，二次排入为空（不重复落库）
    let drained = storage.drain_pending_submissions(session).await.unwrap();
    assert_eq!(drained.len(), 1, "重复 requestId 只排入一次: {drained:?}");
    assert_eq!(drained[0].text(), "二");
    assert!(
        storage
            .drain_pending_submissions(session)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        user_texts(&storage.steps(session).await.unwrap()),
        ["一", "二"]
    );

    // 无待处理提交：收尾转静止且只转一次
    assert!(storage.quiesce_session(session).await.unwrap());
    assert_eq!(
        storage.get_session(session).await.unwrap().unwrap().state(),
        SessionState::Idle
    );

    // 静止后再次受理走立即路径（状态机出口闭合）
    let third = storage
        .record_submission(&submission(session, dir.path(), "三"))
        .await
        .unwrap();
    assert!(matches!(third.acceptance, Acceptance::Accepted { .. }));
    assert!(third.message.is_some());
    assert_eq!(
        storage.get_session(session).await.unwrap().unwrap().state(),
        SessionState::Pending
    );
}

// ---------------------------------------------------------------------------
// 探测 10：删除运行中会话级联清除待处理提交（直查 SQLite）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 删除运行中会话级联清除待处理提交() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nomic.db");
    let storage = SQLiteStorage::open(&db).unwrap();
    let session = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap()
        .id();
    storage
        .set_state(session, SessionState::Running)
        .await
        .unwrap();
    let record = storage
        .record_submission(&submission(session, dir.path(), "挡在运行中"))
        .await
        .unwrap();
    assert!(matches!(record.acceptance, Acceptance::Accepted { .. }));
    assert!(record.message.is_none());

    let pending_count = |conn: &Connection| -> i64 {
        conn.query_row("SELECT COUNT(*) FROM pending_submissions", [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    let conn = Connection::open(&db).unwrap();
    assert_eq!(pending_count(&conn), 1, "运行中受理应留下待处理提交");
    drop(conn);

    storage.delete(session).await.unwrap();
    let conn = Connection::open(&db).unwrap();
    assert_eq!(pending_count(&conn), 0, "删除必须级联清除待处理提交");
}
