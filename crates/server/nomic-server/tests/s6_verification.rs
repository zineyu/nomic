//! 第二轮独立复核（S6 #41）：不看实现者测试，重新推导崩溃恢复、
//! replay 严格性、fork 边界与 turn_completed 快路的行为。

#[path = "common/agent_seam.rs"]
mod agent_seam;

use std::path::Path;

use agent_seam::{ProtocolFixture, ScriptedEnv, ScriptedLlm, ScriptedTurn, wait_for};
use nomic_core::{
    Acceptance, ExecutionEnvironment, Message, MessageId, RequestId, SessionId, SessionState,
    SessionTarget, Step, Storage, Submission, ToolCall, ToolCallId, ToolName, ToolResult,
};
use nomic_server::agent::{
    AgentError, PendingCall, partial_messages, pending_call, replay_messages,
};
use nomic_server::environment::LocalEnvironment;
use nomic_server::storage::SQLiteStorage;

fn read_call(path: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        tool: ToolName::new("read"),
        arguments: serde_json::json!({"path": path}),
    }
}

fn bash_call(command: &str) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        tool: ToolName::new("bash"),
        arguments: serde_json::json!({"command": command}),
    }
}

fn user_step(text: &str) -> Step {
    Step::Message(Message::User {
        id: MessageId::new(),
        text: text.into(),
    })
}

fn assistant_step(calls: Vec<ToolCall>) -> Step {
    Step::Message(Message::Assistant {
        id: MessageId::new(),
        text: String::new(),
        tool_calls: calls,
    })
}

fn specs() -> Vec<nomic_core::ToolSpec> {
    LocalEnvironment::new().tool_specs().to_vec()
}

/// 直接向存储播种：助手消息（含调用声明）已落库、`ToolCallStarted`
/// 未落库，状态仍为运行中（F1 崩溃现场）。
async fn seed_torn(storage: &SQLiteStorage, dir: &Path, calls: Vec<ToolCall>) -> SessionId {
    let session = storage.create_session(dir.to_path_buf()).await.unwrap();
    storage
        .append_step(session.id(), user_step("任务"))
        .await
        .unwrap();
    storage
        .append_step(session.id(), assistant_step(calls))
        .await
        .unwrap();
    storage
        .set_state(session.id(), SessionState::Running)
        .await
        .unwrap();
    session.id()
}

async fn wait_state(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    session: SessionId,
    state: SessionState,
) {
    wait_for(|| async {
        fixture
            .storage
            .get_session(session)
            .await
            .unwrap()
            .is_some_and(|session| session.state() == state)
    })
    .await;
}

async fn steps_of(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    session: SessionId,
) -> Vec<Step> {
    fixture.storage.steps(session).await.unwrap()
}

// ---------------------------------------------------------------------------
// F1 复现：未开始调用的崩溃现场
// ---------------------------------------------------------------------------

/// F1 核心：破损历史（已声明未开始）在恢复面必须被驱动闭合，且严格
/// replay 在恢复后不再报 BrokenHistory。非 replay-safe 工具在此也只需
/// 续跑一次——Started 未落库即副作用未发生。
#[tokio::test]
async fn f1_未开始调用恢复后严格replay可回放且副作用恰好一次() {
    let dir = tempfile::tempdir().unwrap();
    let call = bash_call("make build");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["构建结论"])]);
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("退出码 0"))], specs());
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    // 崩溃现场：该调用从未派发
    let session = seed_torn(&fixture.storage, dir.path(), vec![call.clone()]).await;
    assert!(fixture.env.executions().is_empty(), "播种阶段不得有副作用");
    let before = steps_of(&fixture, session).await;
    assert!(matches!(
        replay_messages(&before),
        Err(AgentError::BrokenHistory(_))
    ));

    let mut client = fixture.restart().await;
    wait_state(&fixture, session, SessionState::Idle).await;

    // 恢复面恰好续跑一次（副作用计数 1），且落库 Started/Finished 各一次
    assert_eq!(fixture.env.executions().len(), 1);
    assert_eq!(fixture.env.executions()[0].call, call);
    let after = steps_of(&fixture, session).await;
    assert_eq!(after[..before.len()], before[..], "崩溃前步骤不得重复落库");
    assert_eq!(
        after
            .iter()
            .filter(|step| matches!(step, Step::ToolCallStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        after
            .iter()
            .filter(|step| matches!(step, Step::ToolCallFinished { .. }))
            .count(),
        1
    );
    // F1 的破损历史已闭合：严格 replay 成功，且 LLM 收到的是完整历史
    assert!(replay_messages(&after).is_ok(), "恢复后历史必须可完整回放");
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 1);
    assert!(
        matches!(
            captured[0].messages.last(),
            Some(Message::Tool {
                result: ToolResult::Ok { output },
                ..
            }) if output == "退出码 0"
        ),
        "LLM 必须看到配对后的工具结果: {:?}",
        captured[0].messages
    );
    // Resume 展示路径同样不失败
    assert_eq!(client.resume(session).await.unwrap().len(), 4);
}

/// replay-safe 工具的未开始调用同样恰好续跑一次。
#[tokio::test]
async fn f1_未开始调用_replay_safe续跑一次() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["内容已收到"])]);
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("文件内容"))], specs());
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let session = seed_torn(&fixture.storage, dir.path(), vec![call.clone()]).await;

    let _client = fixture.restart().await;
    wait_state(&fixture, session, SessionState::Idle).await;

    assert_eq!(fixture.env.executions().len(), 1);
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 1);
    assert!(matches!(
        captured[0].messages.last(),
        Some(Message::Tool { call_id, result: ToolResult::Ok { .. }, .. }) if *call_id == call.id
    ));
}

/// 多调用回合：首个已完成、第二个未开始（崩溃落在两者之间），
/// 恢复只续跑第二个，且严格 replay 成功。
#[tokio::test]
async fn 多调用回合_恢复只续跑未开始的第二个调用() {
    let dir = tempfile::tempdir().unwrap();
    let first = read_call("a.txt");
    let second = bash_call("echo b");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["完成"])]);
    // 只给第二个调用（bash）剧本：若错误地重跑第一个，read 会剧本越界 panic
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("b 输出"))], specs());
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;

    let session = fixture
        .storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    for step in [
        user_step("任务"),
        assistant_step(vec![first.clone(), second.clone()]),
        Step::ToolCallStarted {
            call: first.clone(),
        },
        Step::ToolCallFinished {
            call_id: first.id,
            result: ToolResult::ok("a 内容"),
        },
    ] {
        fixture
            .storage
            .append_step(session.id(), step)
            .await
            .unwrap();
    }
    fixture
        .storage
        .set_state(session.id(), SessionState::Running)
        .await
        .unwrap();

    let mut fixture = fixture;
    let _client = fixture.restart().await;
    wait_state(&fixture, session.id(), SessionState::Idle).await;

    let executions = fixture.env.executions();
    assert_eq!(executions.len(), 1, "只有未开始的第二个调用可续跑");
    assert_eq!(executions[0].call, second);
    let steps = steps_of(&fixture, session.id()).await;
    assert!(
        replay_messages(&steps).is_ok(),
        "两个调用都必须闭合: {steps:?}"
    );
    assert_eq!(
        fixture.llm.captured().len(),
        1,
        "闭合回合后必须请求一次 LLM"
    );
}

// ---------------------------------------------------------------------------
// turn_completed 快路
// ---------------------------------------------------------------------------

/// 助手纯文本回复已落库、会话未释放时崩溃：重启只释放，不得请求 LLM。
#[tokio::test]
async fn 助手回复已落库未释放_重启不再请求_llm() {
    let dir = tempfile::tempdir().unwrap();
    // 空剧本：任何 LLM 请求都会剧本越界 panic
    let llm = ScriptedLlm::new(vec![]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let session = fixture
        .storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    for step in [
        user_step("任务"),
        Step::Message(Message::Assistant {
            id: MessageId::new(),
            text: "完成".into(),
            tool_calls: Vec::new(),
        }),
    ] {
        fixture
            .storage
            .append_step(session.id(), step)
            .await
            .unwrap();
    }
    fixture
        .storage
        .set_state(session.id(), SessionState::Running)
        .await
        .unwrap();
    let before = steps_of(&fixture, session.id()).await;

    let mut fixture = fixture;
    let _client = fixture.restart().await;
    wait_state(&fixture, session.id(), SessionState::Idle).await;

    assert!(
        fixture.llm.captured().is_empty(),
        "已结束回合不得再请求 LLM"
    );
    assert_eq!(steps_of(&fixture, session.id()).await, before);
}

/// 静止会话不进入恢复面；运行中与待处理会话都进入。
#[tokio::test]
async fn 启动恢复只枚举运行中与待处理会话() {
    let dir = tempfile::tempdir().unwrap();
    let storage = SQLiteStorage::open(dir.path().join("nomic.db")).unwrap();
    let idle = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    let running = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    let pending = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    storage
        .set_state(running.id(), SessionState::Running)
        .await
        .unwrap();
    storage
        .set_state(pending.id(), SessionState::Pending)
        .await
        .unwrap();

    let recovered = nomic_server::recovery::recoverable_sessions(&storage)
        .await
        .unwrap();
    assert_eq!(recovered, vec![running.id(), pending.id()]);
    assert_eq!(
        storage
            .get_session(idle.id())
            .await
            .unwrap()
            .unwrap()
            .state(),
        SessionState::Idle,
        "静止会话不得被改写状态"
    );
    assert_eq!(
        storage
            .get_session(running.id())
            .await
            .unwrap()
            .unwrap()
            .state(),
        SessionState::Pending
    );
}

// ---------------------------------------------------------------------------
// 非 replay-safe：interrupted 恢复
// ---------------------------------------------------------------------------

/// Started 已落库（副作用可能已发生）、未闭合的非 replay-safe 调用：
/// 恢复标记 interrupted，副作用计数保持 1，中断说明进入模型可见历史。
#[tokio::test]
async fn 非replay_safe在途调用恢复为interrupted且不重跑() {
    let dir = tempfile::tempdir().unwrap();
    let call = bash_call("make build");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["结论"])]);
    // 第二项确保误重跑会留下副作用计数 2（但不应被消耗）
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("不应发生"))], specs());
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let session = fixture
        .storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    for step in [
        user_step("任务"),
        assistant_step(vec![call.clone()]),
        Step::ToolCallStarted { call: call.clone() },
    ] {
        fixture
            .storage
            .append_step(session.id(), step)
            .await
            .unwrap();
    }
    fixture
        .storage
        .set_state(session.id(), SessionState::Running)
        .await
        .unwrap();
    let before = steps_of(&fixture, session.id()).await;

    let mut fixture = fixture;
    let _client = fixture.restart().await;
    wait_state(&fixture, session.id(), SessionState::Idle).await;

    assert!(
        fixture.env.executions().is_empty(),
        "非 replay-safe 在途调用绝不可重跑"
    );
    let after = steps_of(&fixture, session.id()).await;
    assert_eq!(after[..before.len()], before[..], "崩溃前步骤不得重复落库");
    // 中断步骤紧跟崩溃现场；其后是恢复回合的助手回复
    assert!(matches!(
        &after[before.len()],
        Step::ToolCallInterrupted { call_id, .. } if *call_id == call.id
    ));
    assert_eq!(
        after.len(),
        before.len() + 2,
        "中断 + 一次助手回复: {after:?}"
    );
    assert_eq!(
        after
            .iter()
            .filter(|step| matches!(step, Step::ToolCallStarted { .. }))
            .count(),
        before
            .iter()
            .filter(|step| matches!(step, Step::ToolCallStarted { .. }))
            .count(),
        "非 replay-safe 在途调用恢复不得新增 Started"
    );
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 1);
    assert!(
        matches!(
            captured[0].messages.last(),
            Some(Message::Tool { result: ToolResult::Error { message }, .. })
                if message.contains("interrupted")
        ),
        "中断必须进入模型可见历史: {:?}",
        captured[0].messages
    );
}

// ---------------------------------------------------------------------------
// fork 边界
// ---------------------------------------------------------------------------

/// 分叉点在工具回合之前：分叉点之后的工具步骤不得被复制（无多拷）。
#[tokio::test]
async fn fork_工具回合之前_不复制分叉点之后的工具步骤() {
    let dir = tempfile::tempdir().unwrap();
    let storage = SQLiteStorage::open(dir.path().join("nomic.db")).unwrap();
    let parent = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    let call = read_call("note.txt");
    let m1 = Message::User {
        id: MessageId::new(),
        text: "读文件".into(),
    };
    let assistant = Message::Assistant {
        id: MessageId::new(),
        text: String::new(),
        tool_calls: vec![call.clone()],
    };
    storage
        .append_step(parent.id(), Step::Message(m1.clone()))
        .await
        .unwrap();
    storage
        .append_step(parent.id(), Step::Message(assistant.clone()))
        .await
        .unwrap();
    for step in [
        Step::ToolCallStarted { call: call.clone() },
        Step::ToolCallFinished {
            call_id: call.id,
            result: ToolResult::ok("文件内容"),
        },
    ] {
        storage.append_step(parent.id(), step).await.unwrap();
    }

    let child = storage.fork(parent.id(), assistant.id()).await.unwrap();
    let child_steps = storage.steps(child.id()).await.unwrap();
    assert_eq!(
        child_steps,
        vec![Step::Message(m1), Step::Message(assistant),],
        "分叉点之后的工具步骤不得复制"
    );
    // 该前缀对严格 replay 是破损的（声明未开始），恢复面据此续跑
    assert!(matches!(
        pending_call(&child_steps).unwrap(),
        Some(PendingCall::Unstarted(ref c)) if *c == call
    ));
    assert!(matches!(
        replay_messages(&child_steps),
        Err(AgentError::BrokenHistory(_))
    ));
    assert_eq!(partial_messages(&child_steps).unwrap().len(), 2);
}

/// 分叉首条消息只复制首条；分叉末条消息复制全部。
#[tokio::test]
async fn fork_首条与末条边界() {
    let dir = tempfile::tempdir().unwrap();
    let storage = SQLiteStorage::open(dir.path().join("nomic.db")).unwrap();
    let parent = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    let call = read_call("note.txt");
    let u1 = Message::User {
        id: MessageId::new(),
        text: "一".into(),
    };
    let a1 = Message::Assistant {
        id: MessageId::new(),
        text: String::new(),
        tool_calls: vec![call.clone()],
    };
    let u2 = Message::User {
        id: MessageId::new(),
        text: "二".into(),
    };
    for step in [
        Step::Message(u1.clone()),
        Step::Message(a1.clone()),
        Step::ToolCallStarted { call: call.clone() },
        Step::ToolCallFinished {
            call_id: call.id,
            result: ToolResult::ok("内容"),
        },
        Step::Message(u2.clone()),
    ] {
        storage.append_step(parent.id(), step).await.unwrap();
    }

    let head = storage.fork(parent.id(), u1.id()).await.unwrap();
    assert_eq!(
        storage.steps(head.id()).await.unwrap(),
        vec![Step::Message(u1.clone())]
    );

    let tail = storage.fork(parent.id(), u2.id()).await.unwrap();
    let all = storage.steps(parent.id()).await.unwrap();
    assert_eq!(storage.steps(tail.id()).await.unwrap(), all);
    assert!(replay_messages(&storage.steps(tail.id()).await.unwrap()).is_ok());
}

/// 分叉点在「非 replay-safe 调用已开始、未闭合」之后：子会话继承
/// `ToolCallStarted`，恢复必须中断而非重跑（否则父调用副作用被复制
/// 成两次）。
#[tokio::test]
async fn fork_继承在途非replay_safe调用_子会话中断而不重跑() {
    let dir = tempfile::tempdir().unwrap();
    let call = bash_call("make build");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["分支回复"])]);
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("不应发生"))], specs());
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let parent = fixture
        .storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    let second = Message::User {
        id: MessageId::new(),
        text: "催一下".into(),
    };
    for step in [
        user_step("构建"),
        assistant_step(vec![call.clone()]),
        Step::ToolCallStarted { call: call.clone() },
        Step::Message(second.clone()),
    ] {
        fixture
            .storage
            .append_step(parent.id(), step)
            .await
            .unwrap();
    }
    let child = fixture
        .storage
        .fork(parent.id(), second.id())
        .await
        .unwrap();
    let child_steps = fixture.storage.steps(child.id()).await.unwrap();
    assert!(matches!(
        pending_call(&child_steps).unwrap(),
        Some(PendingCall::InFlight(ref c)) if *c == call
    ));
    assert!(matches!(
        replay_messages(&child_steps),
        Err(AgentError::BrokenHistory(_))
    ));

    let mut client = fixture.connect().await;
    let acceptance = client
        .submit(&Submission {
            request_id: RequestId::new(),
            target: SessionTarget::Existing(child.id()),
            working_dir: dir.path().to_path_buf(),
            content: "新分支".into(),
        })
        .await
        .unwrap();
    assert!(matches!(acceptance, Acceptance::Accepted { .. }));
    wait_state(&fixture, child.id(), SessionState::Idle).await;

    assert!(
        fixture.env.executions().is_empty(),
        "在途非 replay-safe 调用不得因子会话而重跑"
    );
    let after = fixture.storage.steps(child.id()).await.unwrap();
    assert!(
        after.iter().any(
            |step| matches!(step, Step::ToolCallInterrupted { call_id, .. } if *call_id == call.id)
        ),
        "子会话必须把继承的在途调用中断: {after:?}"
    );
    assert!(replay_messages(&after).is_ok(), "中断后子会话历史必须闭合");
}

// ---------------------------------------------------------------------------
// 严格 replay 不得误判合法历史
// ---------------------------------------------------------------------------

fn started(call: &ToolCall) -> Step {
    Step::ToolCallStarted { call: call.clone() }
}

fn finished(call: &ToolCall, output: &str) -> Step {
    Step::ToolCallFinished {
        call_id: call.id,
        result: ToolResult::ok(output),
    }
}

#[test]
fn 合法历史_多轮工具回合与收尾助手消息可回放() {
    let c1 = read_call("a");
    let c2 = bash_call("b");
    let steps = vec![
        user_step("任务"),
        assistant_step(vec![c1.clone(), c2.clone()]),
        started(&c1),
        finished(&c1, "a"),
        started(&c2),
        Step::ToolCallInterrupted {
            call_id: c2.id,
            partial_output: Some("半个日志".into()),
        },
        Step::Message(Message::Assistant {
            id: MessageId::new(),
            text: "收尾".into(),
            tool_calls: Vec::new(),
        }),
        user_step("再来"),
    ];
    let messages = replay_messages(&steps).unwrap();
    // System 不加；User, Assistant, Tool(a), Tool(interrupted), Assistant, User
    assert_eq!(messages.len(), 6);
    assert!(matches!(&messages[3], Message::Tool { call_id, .. } if *call_id == c2.id));
    assert!(pending_call(&steps).unwrap().is_none());
}

#[test]
fn 合法历史_中断后继续并完成后续回合可回放() {
    let c = bash_call("x");
    let steps = vec![
        user_step("任务"),
        assistant_step(vec![c.clone()]),
        started(&c),
        Step::ToolCallInterrupted {
            call_id: c.id,
            partial_output: None,
        },
        Step::Message(Message::Assistant {
            id: MessageId::new(),
            text: "放弃该步".into(),
            tool_calls: Vec::new(),
        }),
    ];
    assert!(replay_messages(&steps).is_ok());
}

#[test]
fn 严格回放_非末尾回合破损仍报错() {
    let c = read_call("a");
    // 上一回合声明未开始就出现下一条助手消息
    let steps = vec![
        assistant_step(vec![c]),
        Step::Message(Message::Assistant {
            id: MessageId::new(),
            text: "跳过".into(),
            tool_calls: Vec::new(),
        }),
    ];
    assert!(matches!(
        replay_messages(&steps),
        Err(AgentError::BrokenHistory(_))
    ));
    assert!(matches!(
        partial_messages(&steps),
        Err(AgentError::BrokenHistory(_))
    ));
}

#[test]
fn pending_call_在途优先于未开始且未开始按声明顺序() {
    let c1 = read_call("a");
    let c2 = read_call("b");
    // 两个调用，c1 已开始未闭合：在途优先
    let in_flight = vec![assistant_step(vec![c1.clone(), c2.clone()]), started(&c1)];
    assert_eq!(
        pending_call(&in_flight).unwrap(),
        Some(PendingCall::InFlight(c1.clone()))
    );
    // c1 闭合后，返回未开始的 c2
    let after_first = vec![
        assistant_step(vec![c1.clone(), c2.clone()]),
        started(&c1),
        finished(&c1, "a"),
    ];
    assert_eq!(
        pending_call(&after_first).unwrap(),
        Some(PendingCall::Unstarted(c2.clone()))
    );
    // 全部闭合：无待处理
    let closed = vec![
        assistant_step(vec![c1.clone(), c2.clone()]),
        started(&c1),
        finished(&c1, "a"),
        started(&c2),
        finished(&c2, "b"),
    ];
    assert_eq!(pending_call(&closed).unwrap(), None);
    assert!(replay_messages(&closed).is_ok());
}
