//! 独立复核（S8 #42）：崩溃恢复路径下延迟受理的落库顺序。

#[path = "common/agent_seam.rs"]
mod agent_seam;

#[path = "common/s8_support.rs"]
mod s8_support;

use agent_seam::{ProtocolFixture, ScriptedEnv, ScriptedLlm, ScriptedTurn, WAIT};
use nomic_core::{
    Acceptance, ExecutionEnvironment, Message, MessageId, SessionTarget, Step, Storage, ToolResult,
};
use nomic_server::environment::LocalEnvironment;
use s8_support::{
    collect_user_texts, empty_reply, read_call, seed_running_session, seeded_subscribed, settle,
    stored_steps, stored_user_texts, submission, submit, submit_new, wait_idle,
};

// ---------------------------------------------------------------------------
// 探测 11：延迟受理后崩溃重启——恢复重驱必须在回合边界排入并执行
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 崩溃重启_运行中受理的待处理消息排入并执行() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["续答"])]);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let session = seed_running_session(
        &fixture,
        dir.path(),
        vec![
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "先".into(),
            }),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: "答".into(),
                tool_calls: Vec::new(),
            }),
        ],
    )
    .await;

    let record = fixture
        .storage
        .record_submission(&submission(session, dir.path(), "后"))
        .await
        .unwrap();
    assert!(matches!(record.acceptance, Acceptance::Accepted { .. }));
    assert!(record.message.is_none());
    assert_eq!(
        stored_steps(&fixture, session).await.len(),
        2,
        "回合进行中不得立即排入步骤"
    );

    let _client = fixture.restart().await;
    wait_idle(&fixture, session).await;

    let steps = stored_steps(&fixture, session).await;
    assert_eq!(
        stored_user_texts(&fixture, session).await,
        ["先", "后"],
        "恢复重驱必须把待处理消息排入会话: {steps:?}"
    );
    assert!(
        matches!(&steps[3], Step::Message(Message::Assistant { text, .. }) if text == "续答"),
        "待处理消息必须被下一回合执行: {steps:?}"
    );
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 1);
    assert!(
        matches!(captured[0].messages.last(), Some(Message::User { text, .. }) if text == "后"),
        "新回合历史必须包含待处理消息: {:?}",
        captured[0].messages
    );
}

#[tokio::test]
async fn 崩溃重启_待处理消息必须排在未闭合工具结果之后() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["读到了"]), empty_reply()]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("文件内容"))], specs);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let session = seed_running_session(
        &fixture,
        dir.path(),
        vec![
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "先".into(),
            }),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: String::new(),
                tool_calls: vec![call.clone()],
            }),
            Step::ToolCallStarted { call: call.clone() },
        ],
    )
    .await;

    let record = fixture
        .storage
        .record_submission(&submission(session, dir.path(), "后"))
        .await
        .unwrap();
    assert!(record.message.is_none());

    let _client = fixture.restart().await;
    wait_idle(&fixture, session).await;

    let steps = stored_steps(&fixture, session).await;
    let finished = steps
        .iter()
        .position(|step| matches!(step, Step::ToolCallFinished { .. }))
        .expect("在途工具调用必须闭合");
    let drained = steps
        .iter()
        .position(|step| matches!(step, Step::Message(Message::User { text, .. }) if text == "后"))
        .expect("待处理消息必须排入");
    assert!(
        drained > finished,
        "待处理用户消息不得插入工具调用中间（回合边界）: {steps:?}"
    );

    // 模型可见历史：助手声明的工具调用之后必须紧跟工具结果
    let messages = &fixture.llm.captured()[0].messages;
    let assistant_at = messages
        .iter()
        .position(
            |message| matches!(message, Message::Assistant { tool_calls, .. } if !tool_calls.is_empty()),
        )
        .expect("历史必须含带工具调用的助手消息");
    assert!(
        matches!(&messages[assistant_at + 1], Message::Tool { .. }),
        "助手工具调用之后必须紧跟工具结果: {messages:?}"
    );
}

#[tokio::test]
async fn 崩溃重启_已声明未开始工具调用也必须先于待处理消息闭合() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["读到了"]), empty_reply()]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("文件内容"))], specs);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let session = seed_running_session(
        &fixture,
        dir.path(),
        vec![
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "先".into(),
            }),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: String::new(),
                tool_calls: vec![call.clone()],
            }),
        ],
    )
    .await;

    let record = fixture
        .storage
        .record_submission(&submission(session, dir.path(), "后"))
        .await
        .unwrap();
    assert!(record.message.is_none());

    let _client = fixture.restart().await;
    wait_idle(&fixture, session).await;

    let steps = stored_steps(&fixture, session).await;
    let finished = steps
        .iter()
        .position(|step| matches!(step, Step::ToolCallFinished { .. }))
        .expect("未开始工具调用必须续跑");
    let drained = steps
        .iter()
        .position(|step| matches!(step, Step::Message(Message::User { text, .. }) if text == "后"))
        .expect("待处理消息必须排入");
    assert!(
        drained > finished,
        "待处理用户消息不得插入工具调用中间（回合边界）: {steps:?}"
    );
}

// ---------------------------------------------------------------------------
// 探测 14：崩溃恢复后工具闭合与待处理消息各消耗一个回合，顺序与请求数钉死
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 崩溃恢复_工具闭合后待处理消息恰好消耗一个回合() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::reply(["读到了"]),
        ScriptedTurn::reply(["后答"]),
    ]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("文件内容"))], specs);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let session = seed_running_session(
        &fixture,
        dir.path(),
        vec![
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "先".into(),
            }),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: String::new(),
                tool_calls: vec![call.clone()],
            }),
            Step::ToolCallStarted { call: call.clone() },
        ],
    )
    .await;
    let record = fixture
        .storage
        .record_submission(&submission(session, dir.path(), "后"))
        .await
        .unwrap();
    assert!(record.message.is_none());

    let _client = fixture.restart().await;
    wait_idle(&fixture, session).await;

    // 精确步骤序列：工具结果 → 工具回合助手 → 待处理用户 → 新回合助手
    let steps = stored_steps(&fixture, session).await;
    assert_eq!(steps.len(), 7, "不得重复落步或丢步: {steps:?}");
    assert!(matches!(&steps[0], Step::Message(Message::User { text, .. }) if text == "先"));
    assert!(matches!(
        &steps[1],
        Step::Message(Message::Assistant { tool_calls, .. }) if tool_calls.len() == 1
    ));
    assert!(matches!(&steps[2], Step::ToolCallStarted { .. }));
    assert!(matches!(&steps[3], Step::ToolCallFinished { .. }));
    assert!(
        matches!(&steps[4], Step::Message(Message::Assistant { text, .. }) if text == "读到了")
    );
    assert!(matches!(&steps[5], Step::Message(Message::User { text, .. }) if text == "后"));
    assert!(matches!(&steps[6], Step::Message(Message::Assistant { text, .. }) if text == "后答"));

    // 恰好两次请求：闭合后的续跑与待处理消息的回合各一次
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 2, "不得多请求或漏请求");
    assert!(
        matches!(captured[0].messages.last(), Some(Message::Tool { .. })),
        "第一次请求必须以工具结果结尾: {:?}",
        captured[0].messages
    );
    assert!(
        matches!(captured[1].messages.last(), Some(Message::User { text, .. }) if text == "后"),
        "第二次请求必须回答待处理消息: {:?}",
        captured[1].messages
    );
}

// ---------------------------------------------------------------------------
// 探测 12：同一到达序列（工具在途时受理）在不停机路径下必须排在工具结果之后
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 工具在途时受理_正常运行路径排在工具结果之后() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let tool_gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![
        empty_reply(),
        ScriptedTurn::Play(vec![agent_seam::ScriptedEvent::ToolCall(call.clone())]),
        ScriptedTurn::reply(["读到了"]),
        ScriptedTurn::reply(["问题答案"]),
    ]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::gated(
        vec![Ok(ToolResult::ok("文件内容"))],
        specs,
        tool_gate.clone(),
    );
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut a = fixture.connect().await;
    let mut b = fixture.connect().await;
    let session = seeded_subscribed(&fixture, &mut a, &mut b, dir.path()).await;

    assert!(matches!(
        submit(&mut a, SessionTarget::Existing(session), dir.path(), "一").await,
        Acceptance::Accepted { .. }
    ));
    // 工具已派发但未返回：回合仍在进行中
    assert!(settle(WAIT, || async { !fixture.env.executions().is_empty() }).await);
    assert!(matches!(
        submit(&mut b, SessionTarget::Existing(session), dir.path(), "二").await,
        Acceptance::Accepted { .. }
    ));
    assert_eq!(stored_user_texts(&fixture, session).await, ["种子", "一"]);

    tool_gate.release();
    wait_idle(&fixture, session).await;

    // 回合边界 drain：用户“二”排在回合（含工具结果与助手“读到了”）之后
    let steps = stored_steps(&fixture, session).await;
    let finished = steps
        .iter()
        .position(|step| matches!(step, Step::ToolCallFinished { .. }))
        .expect("工具调用必须闭合");
    let drained = steps
        .iter()
        .position(|step| matches!(step, Step::Message(Message::User { text, .. }) if text == "二"))
        .expect("待处理消息必须排入");
    assert!(
        drained > finished,
        "正常运行路径必须把用户消息排在工具结果之后: {steps:?}"
    );
    assert_eq!(
        stored_user_texts(&fixture, session).await,
        ["种子", "一", "二"]
    );
}

// ---------------------------------------------------------------------------
// 探测 13：一次运行中连续 defer 的排入顺序必须等于到达顺序，且两连接一致
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 一次运行中连续延迟受理_排入顺序等于到达顺序() {
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

    // 第一条触发运行并被闸门钉住
    assert!(matches!(
        submit(&mut a, SessionTarget::Existing(session), dir.path(), "一").await,
        Acceptance::Accepted { .. }
    ));
    assert!(settle(WAIT, || async { fixture.llm.captured().len() == 2 }).await);

    // 运行中连续受理三条：全部延迟落库，到达顺序：二、三、四
    for text in ["二", "三", "四"] {
        assert!(matches!(
            submit(&mut b, SessionTarget::Existing(session), dir.path(), text).await,
            Acceptance::Accepted { .. }
        ));
    }
    assert_eq!(stored_user_texts(&fixture, session).await, ["种子", "一"]);

    gate.release_all(2);
    wait_idle(&fixture, session).await;

    assert_eq!(
        stored_user_texts(&fixture, session).await,
        ["种子", "一", "二", "三", "四"]
    );
    let expected = ["一", "二", "三", "四"];
    assert_eq!(collect_user_texts(&mut a, 4).await, expected);
    assert_eq!(collect_user_texts(&mut b, 4).await, expected);
}

// ---------------------------------------------------------------------------
// 探测 7：Resume 回放边界不得混入其他会话的实时消息
// ---------------------------------------------------------------------------

#[tokio::test]
async fn 回放不得混入其他会话的实时消息() {
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

    // 第二个会话产生实时消息（用户 + 增量 + 完成）
    assert!(matches!(
        submit(&mut a, SessionTarget::Existing(second), dir.path(), "丙").await,
        Acceptance::Accepted { .. }
    ));
    wait_idle(&fixture, second).await;
    assert_eq!(fixture.llm.captured().len(), 3);

    // 实时消息已在连接流中；此时回放第一个会话
    let replayed = a.resume(first).await.unwrap();
    let texts: Vec<String> = replayed.iter().map(|m| m.text().to_string()).collect();
    assert_eq!(texts, ["甲", ""], "回放只含第一个会话的历史: {texts:?}");
}
