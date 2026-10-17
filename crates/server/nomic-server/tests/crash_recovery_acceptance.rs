//! 崩溃恢复与 replay 语义 seam 验收（#41，S6）。
//!
//! 模拟 server 在工具执行前/后崩溃：重启后待处理会话自动重驱，
//! replay-safe 工具恰好续跑一次；非 replay-safe 工具恢复后为
//! interrupted、模型可见历史含中断通知、副作用计数为 1；
//! 崩溃前已完成的步骤不重复落库；静止会话不被无谓唤醒。
//!
//! 崩溃经 [`ProtocolFixture::restart`] 模拟：旧实例的服务循环停止、
//! 在途 worker 永久挂起（等价于进程死亡），新实例在同一个 SQLite
//! 文件上做启动恢复。

#[path = "common/agent_seam.rs"]
mod agent_seam;

use std::path::Path;

use agent_seam::{
    ProtocolFixture, ScriptedEnv, ScriptedEvent, ScriptedLlm, ScriptedOutcome, ScriptedTurn,
    wait_for,
};
use nomic_core::{
    Acceptance, ExecutionEnvironment, LlmError, Message, MessageId, RequestId, SessionId,
    SessionState, SessionTarget, Step, Storage, Submission, ToolCall, ToolCallId, ToolName,
    ToolResult,
};
use nomic_server::environment::LocalEnvironment;

/// 提交一条用户消息到新会话，返回受理的会话 id。
async fn submit_new(
    client: &mut nomic_server::client::Client,
    dir: &Path,
    content: &str,
) -> SessionId {
    let acceptance = client
        .submit(&Submission {
            request_id: RequestId::new(),
            target: SessionTarget::New,
            working_dir: dir.to_path_buf(),
            content: content.into(),
        })
        .await
        .unwrap();
    match acceptance {
        Acceptance::Accepted { session_id } => session_id,
        Acceptance::Duplicate { .. } => panic!("首次提交不应判重"),
    }
}

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

/// 等待会话到达给定状态。
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

/// 读取会话落库步骤。
async fn steps_of(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    session: SessionId,
) -> Vec<Step> {
    fixture.storage.steps(session).await.unwrap()
}

/// 构造崩溃现场：助手消息（含工具调用请求）已 checkpoint，
/// `ToolCallStarted` 未落库（崩溃落在两次写盘之间），状态仍为运行中。
async fn seed_torn_turn(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    dir: &Path,
    calls: Vec<ToolCall>,
) -> SessionId {
    let session = fixture
        .storage
        .create_session(dir.to_path_buf())
        .await
        .unwrap();
    fixture
        .storage
        .append_step(
            session.id(),
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "完成任务".into(),
            }),
        )
        .await
        .unwrap();
    fixture
        .storage
        .append_step(
            session.id(),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: String::new(),
                tool_calls: calls,
            }),
        )
        .await
        .unwrap();
    fixture
        .storage
        .set_state(session.id(), SessionState::Running)
        .await
        .unwrap();
    session.id()
}

/// 构造崩溃现场：回合已在无工具调用的助手消息处结束，
/// worker 尚未释放会话（状态仍为运行中）。
async fn seed_completed_turn(
    fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>,
    dir: &Path,
) -> SessionId {
    let session = fixture
        .storage
        .create_session(dir.to_path_buf())
        .await
        .unwrap();
    fixture
        .storage
        .append_step(
            session.id(),
            Step::Message(Message::User {
                id: MessageId::new(),
                text: "完成任务".into(),
            }),
        )
        .await
        .unwrap();
    fixture
        .storage
        .append_step(
            session.id(),
            Step::Message(Message::Assistant {
                id: MessageId::new(),
                text: "完成".into(),
                tool_calls: Vec::new(),
            }),
        )
        .await
        .unwrap();
    fixture
        .storage
        .set_state(session.id(), SessionState::Running)
        .await
        .unwrap();
    session.id()
}

#[tokio::test]
async fn 执行前崩溃_replay_safe工具重启后续跑恰好一次() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Play(vec![
            ScriptedEvent::Token("先读文件".into()),
            ScriptedEvent::ToolCall(call.clone()),
        ]),
        ScriptedTurn::reply(["内容已收到"]),
    ]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(
        vec![
            ScriptedOutcome::CrashBeforeEffect,
            ScriptedOutcome::Return(Ok(ToolResult::ok("文件内容"))),
        ],
        specs,
    );
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "帮我看下 note.txt").await;

    // 崩溃现场：调用已开始落库，副作用未发生（剧本在派发前挂起）
    wait_for(|| async {
        steps_of(&fixture, session_id)
            .await
            .iter()
            .any(|step| matches!(step, Step::ToolCallStarted { .. }))
    })
    .await;
    let steps_before = steps_of(&fixture, session_id).await;
    assert!(
        fixture.env.executions().is_empty(),
        "执行前崩溃不得记录副作用"
    );

    let mut client = fixture.restart().await;
    wait_state(&fixture, session_id, SessionState::Idle).await;

    // replay-safe 工具恰好续跑一次：副作用计数为 1，结果落库一次
    let executions = fixture.env.executions();
    assert_eq!(executions.len(), 1, "read 恰好续跑一次");
    assert_eq!(executions[0].call, call);
    let steps_after = steps_of(&fixture, session_id).await;
    assert_eq!(
        steps_after[..steps_before.len()],
        steps_before[..],
        "崩溃前已完成步骤不重复落库"
    );
    assert_eq!(
        steps_after
            .iter()
            .filter(|step| matches!(step, Step::ToolCallStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        steps_after
            .iter()
            .filter(|step| matches!(step, Step::ToolCallFinished { .. }))
            .count(),
        1
    );
    // 续跑结果经完整回放进入模型可见历史
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 2);
    assert!(matches!(
        captured[1].messages.last(),
        Some(Message::Tool {
            result: ToolResult::Ok { output },
            ..
        }) if output == "文件内容"
    ));
    // 客户端 Resume 看到完整历史（含续跑结果）
    let history = client.resume(session_id).await.unwrap();
    assert_eq!(history.len(), 4);
}

#[tokio::test]
async fn 执行后崩溃_replay_safe工具恰好续跑一次() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Play(vec![ScriptedEvent::ToolCall(call.clone())]),
        ScriptedTurn::reply(["内容已收到"]),
    ]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(
        vec![
            ScriptedOutcome::CrashAfterEffect,
            ScriptedOutcome::Return(Ok(ToolResult::ok("文件内容"))),
        ],
        specs,
    );
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "帮我看下 note.txt").await;
    wait_for(|| async { !fixture.env.executions().is_empty() }).await;

    let _client = fixture.restart().await;
    wait_state(&fixture, session_id, SessionState::Idle).await;

    // 已发生的调用恰好续跑一次：副作用计数 2（崩溃前 1 次 + 续跑 1 次），
    // 结果步骤只落库一次
    assert_eq!(fixture.env.executions().len(), 2, "read 恰好续跑一次");
    let steps = steps_of(&fixture, session_id).await;
    assert_eq!(
        steps
            .iter()
            .filter(|step| matches!(step, Step::ToolCallStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        steps
            .iter()
            .filter(|step| matches!(step, Step::ToolCallFinished { .. }))
            .count(),
        1
    );
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, Step::ToolCallInterrupted { .. }))
    );
}

#[tokio::test]
async fn 执行后崩溃_非replay_safe工具恢复为interrupted且副作用不重复() {
    let dir = tempfile::tempdir().unwrap();
    let call = bash_call("make build");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Play(vec![
            ScriptedEvent::Token("开始构建".into()),
            ScriptedEvent::ToolCall(call.clone()),
        ]),
        ScriptedTurn::reply(["构建结论已收到"]),
    ]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(
        vec![
            ScriptedOutcome::CrashAfterEffect,
            // 恢复若误重跑会消耗本项，副作用计数随之暴露
            ScriptedOutcome::Return(Ok(ToolResult::ok("不应发生"))),
        ],
        specs,
    );
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "跑一次构建").await;

    // 崩溃现场：副作用已发生（派发已记录），结果未落库
    wait_for(|| async { !fixture.env.executions().is_empty() }).await;
    let steps_before = steps_of(&fixture, session_id).await;
    assert!(matches!(
        steps_before.last(),
        Some(Step::ToolCallStarted { .. })
    ));

    let _client = fixture.restart().await;
    wait_state(&fixture, session_id, SessionState::Idle).await;

    // 副作用恰好一次：恢复不重跑非 replay-safe 工具
    assert_eq!(fixture.env.executions().len(), 1);
    let steps_after = steps_of(&fixture, session_id).await;
    assert_eq!(
        steps_after[..steps_before.len()],
        steps_before[..],
        "崩溃前已完成步骤不重复落库"
    );
    assert!(matches!(
        &steps_after[steps_before.len()],
        Step::ToolCallInterrupted {
            call_id,
            partial_output: None,
        } if *call_id == call.id
    ));
    assert_eq!(
        steps_after
            .iter()
            .filter(|step| matches!(step, Step::ToolCallInterrupted { .. }))
            .count(),
        1
    );
    // 中断通知在模型可见历史中
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 2);
    assert!(matches!(
        captured[1].messages.last(),
        Some(Message::Tool {
            result: ToolResult::Error { message },
            ..
        }) if message.contains("interrupted")
    ));
}

#[tokio::test]
async fn 待处理会话重启后自动重驱并从最后checkpoint继续() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Fail(LlmError::Transport("连接中断".into())),
        ScriptedTurn::reply(["恢复后继续"]),
    ]);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "继续任务").await;

    // 失败挂回待处理后无人重驱：会话静置
    wait_state(&fixture, session_id, SessionState::Pending).await;
    let steps_before = steps_of(&fixture, session_id).await;
    assert_eq!(fixture.llm.captured().len(), 1);

    let _client = fixture.restart().await;
    wait_state(&fixture, session_id, SessionState::Idle).await;

    // 重启恢复把待处理工作入队：从最后 checkpoint 继续
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 2, "待处理会话被自动重驱");
    assert_eq!(captured[1].session, session_id);
    let steps_after = steps_of(&fixture, session_id).await;
    assert_eq!(
        steps_after[..steps_before.len()],
        steps_before[..],
        "崩溃前已完成步骤不重复落库"
    );
    assert_eq!(steps_after.len(), steps_before.len() + 1);
    assert!(matches!(
        &steps_after[steps_before.len()],
        Step::Message(message) if message.text() == "恢复后继续"
    ));
}

#[tokio::test]
async fn 已静止会话重启后不被无谓唤醒() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::reply(["完成"]),
        ScriptedTurn::reply(["探针完成"]),
        ScriptedTurn::reply(["无谓请求"]),
    ]);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let quiet = submit_new(&mut client, dir.path(), "完成任务").await;
    wait_state(&fixture, quiet, SessionState::Idle).await;
    let steps_before = steps_of(&fixture, quiet).await;
    assert_eq!(fixture.llm.captured().len(), 1);

    let mut client = fixture.restart().await;
    // 探针会话在串行调度器上完成，说明恢复入队已全部处理完毕；
    // 若静止会话被误入队，会在探针之前消耗一次请求
    let probe = submit_new(&mut client, dir.path(), "探针").await;
    wait_state(&fixture, probe, SessionState::Idle).await;

    assert_eq!(fixture.llm.captured().len(), 2, "静止会话不得被重启唤醒");
    let captured = fixture.llm.captured();
    assert_eq!(captured[0].session, quiet);
    assert_eq!(captured[1].session, probe);
    assert_eq!(steps_of(&fixture, quiet).await, steps_before);
}

#[tokio::test]
async fn 助手消息已落库未开始时崩溃_replay_safe调用续跑一次() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["内容已收到"])]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("文件内容"))], specs);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let session_id = seed_torn_turn(&fixture, dir.path(), vec![call.clone()]).await;

    let mut client = fixture.restart().await;
    wait_state(&fixture, session_id, SessionState::Idle).await;

    // 声明已 checkpoint、开始未 checkpoint ⇒ 未派发过，恰好续跑一次
    assert_eq!(fixture.env.executions().len(), 1);
    let steps = steps_of(&fixture, session_id).await;
    assert_eq!(steps.len(), 5, "步骤: {steps:?}");
    assert!(matches!(&steps[2], Step::ToolCallStarted { call: started } if *started == call));
    assert!(matches!(
        &steps[3],
        Step::ToolCallFinished { call_id, .. } if *call_id == call.id
    ));
    // 模型历史完整：助手声明的调用有结果
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 1);
    assert!(matches!(
        captured[0].messages.last(),
        Some(Message::Tool {
            result: ToolResult::Ok { output },
            ..
        }) if output == "文件内容"
    ));
    // 客户端 Resume 不因先前的中间态失败
    let history = client.resume(session_id).await.unwrap();
    assert_eq!(history.len(), 4);
}

#[tokio::test]
async fn 助手消息已落库未开始时崩溃_非replay_safe调用续跑副作用一次() {
    let dir = tempfile::tempdir().unwrap();
    let call = bash_call("make build");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["构建完成"])]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("退出码 0"))], specs);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let session_id = seed_torn_turn(&fixture, dir.path(), vec![call.clone()]).await;

    let _client = fixture.restart().await;
    wait_state(&fixture, session_id, SessionState::Idle).await;

    // Started 未落库 ⇒ 未派发，副作用未发生；续跑后计数为 1，不是重复执行
    assert_eq!(fixture.env.executions().len(), 1);
    let steps = steps_of(&fixture, session_id).await;
    assert!(matches!(
        &steps[3],
        Step::ToolCallFinished { call_id, .. } if *call_id == call.id
    ));
    assert!(
        !steps
            .iter()
            .any(|step| matches!(step, Step::ToolCallInterrupted { .. }))
    );
}

#[tokio::test]
async fn 助手回复已落库未释放时崩溃_重启直接释放不再请求() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["不应被请求"])]);
    let mut fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let session_id = seed_completed_turn(&fixture, dir.path()).await;

    let _client = fixture.restart().await;
    wait_state(&fixture, session_id, SessionState::Idle).await;

    // 回合已在助手消息处结束：重驱不得再请求一次 LLM
    assert!(
        fixture.llm.captured().is_empty(),
        "已结束回合不得被重驱唤醒"
    );
    assert_eq!(steps_of(&fixture, session_id).await.len(), 2);
}

#[tokio::test]
async fn 分叉子会话不重跑父会话已完成的工具调用() {
    let dir = tempfile::tempdir().unwrap();
    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["内容已收到"])]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;

    // 父会话：完整工具回合之后继续一条用户消息
    let parent = fixture
        .storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    let first = Message::User {
        id: MessageId::new(),
        text: "读文件".into(),
    };
    let assistant = Message::Assistant {
        id: MessageId::new(),
        text: String::new(),
        tool_calls: vec![call.clone()],
    };
    let second = Message::User {
        id: MessageId::new(),
        text: "继续".into(),
    };
    for step in [
        Step::Message(first),
        Step::Message(assistant),
        Step::ToolCallStarted { call: call.clone() },
        Step::ToolCallFinished {
            call_id: call.id,
            result: ToolResult::ok("文件内容"),
        },
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

    // 父会话的工具调用属于共享历史：子会话不重跑副作用
    assert!(
        fixture.env.executions().is_empty(),
        "子会话不得重跑父会话已完成的调用"
    );
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 1);
    assert!(matches!(
        captured[0].messages.as_slice(),
        [
            Message::System { .. },
            Message::User { .. },
            Message::Assistant { .. },
            Message::Tool { .. },
            Message::User { .. },
            Message::User { .. },
        ]
    ));
}
