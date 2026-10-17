//! 工具调用回合 seam 验收（#38）：剧本驱动「对话→工具调用→执行环境
//! 执行→结果回灌→后续推理」完整闭环，事件与 checkpoint 步骤一一对应；
//! 三层结果回灌、checkpoint 先行顺序、历史回放一致性、
//! interrupted 部分输出回灌、环境派发故障路径。

#[path = "common/agent_seam.rs"]
mod agent_seam;

mod common;

use agent_seam::{
    CapturedExecution, ScriptedEnv, ScriptedEvent, ScriptedLlm, ScriptedTurn, run_collect,
};
use nomic_core::{
    Acceptance, EnvironmentError, Event, ExecutionEnvironment, Message, RequestId, Role,
    SessionState, SessionTarget, Step, Storage, Submission, ToolCall, ToolCallId, ToolName,
    ToolResult,
};
use nomic_server::agent::{AgentError, Worker};
use nomic_server::environment::LocalEnvironment;

use crate::common::open_storage;

/// 提交一条用户消息到新会话，返回受理的会话 id。
async fn submit_new(
    storage: &impl Storage,
    dir: &std::path::Path,
    content: &str,
) -> nomic_core::SessionId {
    let acceptance = storage
        .record_submission(&Submission {
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

#[tokio::test]
async fn 剧本驱动工具回合完整闭环_事件与落库步骤一一对应() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit_new(&storage, dir.path(), "帮我看下 note.txt").await;

    let call = read_call("note.txt");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Play(vec![
            ScriptedEvent::Token("我先读文件".into()),
            ScriptedEvent::ToolCall(call.clone()),
        ]),
        ScriptedTurn::reply(["内容已收到"]),
    ]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("文件内容"))], specs.clone());

    let (ran, events) = run_collect(&storage, &llm, &env).await;
    assert_eq!(ran.unwrap(), Some(session_id));

    let finished = assert_event_sequence(&events, &call);
    assert_step_sequence(&storage, session_id, &call, finished).await;
    assert_request_sequence(&llm, &call, &specs);
    assert_dispatch(&env, session_id, dir.path(), &call);

    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

/// 事件序列：助手（含调用）→ 调用开始 → 调用结束 → 回灌完成 → 续跑回复。
fn assert_event_sequence<'a>(events: &'a [Event], call: &ToolCall) -> &'a ToolResult {
    match events {
        [
            Event::TokenDelta { delta, .. },
            Event::MessageCompleted {
                message: assistant, ..
            },
            Event::ToolCallStarted { call: started, .. },
            Event::ToolCallFinished {
                call_id, result, ..
            },
            Event::MessageCompleted {
                message: feedback, ..
            },
            Event::TokenDelta { delta: tail, .. },
            Event::MessageCompleted { message: reply, .. },
        ] => {
            assert_eq!(delta, "我先读文件");
            assert_eq!(assistant.text(), "我先读文件");
            assert_eq!(assistant.tool_calls(), std::slice::from_ref(call));
            assert_eq!(*started, *call);
            assert_eq!(*call_id, call.id);
            assert!(matches!(result, ToolResult::Ok { output } if output == "文件内容"));
            assert_eq!(feedback.role(), Role::Tool);
            assert_eq!(feedback.text(), "文件内容");
            assert_eq!(tail, "内容已收到");
            assert_eq!(reply.text(), "内容已收到");
            assert!(reply.tool_calls().is_empty());
            result
        }
        other => panic!("事件序列与剧本不符: {other:?}"),
    }
}

/// 落库步骤与事件一一对应；回灌消息不落库（回放从配对步骤构造）。
async fn assert_step_sequence(
    storage: &impl Storage,
    session_id: nomic_core::SessionId,
    call: &ToolCall,
    finished: &ToolResult,
) {
    let steps = storage.steps(session_id).await.unwrap();
    match &steps[..] {
        [
            Step::Message(user),
            Step::Message(assistant),
            Step::ToolCallStarted { call: started },
            Step::ToolCallFinished { call_id, result },
            Step::Message(reply),
        ] => {
            assert_eq!(user.role(), Role::User);
            assert_eq!(assistant.tool_calls(), std::slice::from_ref(call));
            assert_eq!(started, call);
            assert_eq!(*call_id, call.id);
            assert_eq!(result, finished);
            assert_eq!(reply.role(), Role::Assistant);
        }
        other => panic!("落库步骤与事件不符: {other:?}"),
    }
}

/// 请求历史：首轮 [系统, 用户] 且携带注册表声明；次轮含工具结果回灌。
fn assert_request_sequence(llm: &ScriptedLlm, call: &ToolCall, specs: &[nomic_core::ToolSpec]) {
    let captured = llm.captured();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].tools, specs);
    match &captured[0].messages[..] {
        [Message::System { .. }, Message::User { text, .. }] => {
            assert_eq!(text, "帮我看下 note.txt");
        }
        other => panic!("首轮请求历史不符: {other:?}"),
    }
    match &captured[1].messages[..] {
        [
            Message::System { .. },
            Message::User { .. },
            Message::Assistant { .. },
            Message::Tool {
                call_id, result, ..
            },
        ] => {
            assert_eq!(*call_id, call.id);
            assert!(matches!(result, ToolResult::Ok { output } if output == "文件内容"));
        }
        other => panic!("次轮请求历史（回灌）不符: {other:?}"),
    }
}

/// 派发快照：工作目录坐标与会话标识随调用到达执行环境。
fn assert_dispatch(
    env: &ScriptedEnv,
    session_id: nomic_core::SessionId,
    working_dir: &std::path::Path,
    call: &ToolCall,
) {
    match &env.executions()[..] {
        [
            CapturedExecution {
                session,
                working_dir: dispatched_dir,
                call: executed,
            },
        ] => {
            assert_eq!(*session, session_id);
            assert_eq!(dispatched_dir, working_dir);
            assert_eq!(executed, call);
        }
        other => panic!("派发快照不符: {other:?}"),
    }
}

#[tokio::test]
async fn 三层结果各自作为工具消息回灌模型() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit_new(&storage, dir.path(), "跑三个工具").await;

    let ok_call = bash_call("echo hi");
    let error_call = read_call("missing.txt");
    let rejected_call = bash_call("rm x");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Play(vec![
            ScriptedEvent::ToolCall(ok_call.clone()),
            ScriptedEvent::ToolCall(error_call.clone()),
            ScriptedEvent::ToolCall(rejected_call.clone()),
        ]),
        ScriptedTurn::reply(["完成"]),
    ]);
    let env = ScriptedEnv::new(
        vec![
            Ok(ToolResult::ok("标准输出")),
            Ok(ToolResult::error("文件不存在")),
            Ok(ToolResult::rejected("结构化拒绝原因：命令被策略拒绝")),
        ],
        Vec::new(),
    );

    let (ran, _) = run_collect(&storage, &llm, &env).await;
    assert_eq!(ran.unwrap(), Some(session_id));

    // 落库：三次调用各自开始/结束步骤，结果保持三层
    let steps = storage.steps(session_id).await.unwrap();
    let finished: Vec<&ToolResult> = steps
        .iter()
        .filter_map(|step| match step {
            Step::ToolCallFinished { result, .. } => Some(result),
            _ => None,
        })
        .collect();
    assert!(matches!(finished[0], ToolResult::Ok { output } if output == "标准输出"));
    assert!(matches!(finished[1], ToolResult::Error { message } if message == "文件不存在"));
    assert!(
        matches!(finished[2], ToolResult::Rejected { reason } if reason.contains("结构化拒绝原因"))
    );

    // 回灌：第二轮请求历史含三条工具消息，顺序与调用一致
    let captured = llm.captured();
    assert_eq!(captured.len(), 2);
    let tool_messages: Vec<&Message> = captured[1]
        .messages
        .iter()
        .filter(|message| message.role() == Role::Tool)
        .collect();
    assert_eq!(tool_messages.len(), 3);
    assert_eq!(tool_messages[0].text(), "标准输出");
    assert_eq!(tool_messages[1].text(), "文件不存在");
    assert!(tool_messages[2].text().contains("结构化拒绝原因"));

    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 工具回合_checkpoint_先行顺序钉扎() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit_new(&storage, dir.path(), "顺序").await;

    let call = read_call("a.txt");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Play(vec![ScriptedEvent::ToolCall(call.clone())]),
        ScriptedTurn::reply(["回"]),
    ]);
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("内容"))], Vec::new());

    // 事件 sink 在事件到达时即刻回读存储，钉住逐步 checkpoint 顺序
    let mut steps_at_completed_assistant = Vec::new();
    let mut steps_at_started = Vec::new();
    let mut steps_at_finished = Vec::new();
    let mut steps_at_feedback = Vec::new();
    {
        let mut emit = |event: Event| match event {
            Event::MessageCompleted { ref message, .. } if !message.tool_calls().is_empty() => {
                steps_at_completed_assistant =
                    futures::executor::block_on(storage.steps(session_id)).unwrap();
            }
            Event::ToolCallStarted { .. } => {
                steps_at_started = futures::executor::block_on(storage.steps(session_id)).unwrap();
            }
            Event::ToolCallFinished { .. } => {
                steps_at_finished = futures::executor::block_on(storage.steps(session_id)).unwrap();
            }
            Event::MessageCompleted { ref message, .. } if message.role() == Role::Tool => {
                steps_at_feedback = futures::executor::block_on(storage.steps(session_id)).unwrap();
            }
            _ => {}
        };
        Worker::new(&storage, &llm, &env)
            .run_next_pending(&mut emit)
            .await
            .unwrap();
    }

    // 助手消息完成事件发出时必须已落库（含工具调用请求）
    assert!(
        matches!(
            steps_at_completed_assistant.last(),
            Some(Step::Message(Message::Assistant { tool_calls, .. })) if !tool_calls.is_empty()
        ),
        "助手步骤必须先于完成事件落库: {steps_at_completed_assistant:?}"
    );
    // 调用开始事件发出时必须已落库，且执行尚未产生结束步骤
    assert!(
        matches!(steps_at_started.last(), Some(Step::ToolCallStarted { .. })),
        "开始步骤必须先于开始事件落库: {steps_at_started:?}"
    );
    assert_eq!(steps_at_started.len(), 3);
    // 调用完成事件发出时必须已落库
    assert!(
        matches!(
            steps_at_finished.last(),
            Some(Step::ToolCallFinished { .. })
        ),
        "结束步骤必须先于完成事件落库: {steps_at_finished:?}"
    );
    // 回灌：完成事件对应的结果步骤必须已落库（回灌消息由回放构造，不落库）
    assert!(
        matches!(
            steps_at_feedback.last(),
            Some(Step::ToolCallFinished { .. })
        ),
        "结果步骤必须先于回灌完成事件落库: {steps_at_feedback:?}"
    );

    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 历史回放_重驱请求序列与落库步骤一致() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit_new(&storage, dir.path(), "第一段").await;

    let call = read_call("a.txt");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Play(vec![ScriptedEvent::ToolCall(call.clone())]),
        ScriptedTurn::reply(["第一段完成"]),
        ScriptedTurn::reply(["第二段完成"]),
    ]);
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("内容A"))], Vec::new());

    let (ran, _) = run_collect(&storage, &llm, &env).await;
    assert_eq!(ran.unwrap(), Some(session_id));
    submit_existing(&storage, session_id, "第二段").await;

    let (ran, _) = run_collect(&storage, &llm, &env).await;
    assert_eq!(ran.unwrap(), Some(session_id));

    // 第二轮运行的请求历史 = 系统提示 + 落库步骤的显式回放
    let captured = llm.captured();
    assert_eq!(captured.len(), 3);
    let transcript = |messages: &[Message]| {
        messages
            .iter()
            .map(|message| (message.role(), message.text().to_string()))
            .collect::<Vec<_>>()
    };
    // 期望序列：回放把 started+finished 配对构造为一条工具结果消息。
    // 第三次 respond 在最终回复之前，历史止于第二段用户消息。
    let expected: Vec<(Role, String)> = vec![
        (Role::System, String::new()),
        (Role::User, "第一段".into()),
        (Role::Assistant, String::new()),
        (Role::Tool, "内容A".into()),
        (Role::Assistant, "第一段完成".into()),
        (Role::User, "第二段".into()),
    ];

    let actual = transcript(&captured[2].messages);
    assert_eq!(actual.len(), expected.len());
    for (index, ((role, text), (expected_role, expected_text))) in
        actual.iter().zip(&expected).enumerate()
    {
        assert_eq!(role, expected_role, "第 {index} 条角色不符");
        if *role != Role::System {
            assert_eq!(text, expected_text, "第 {index} 条文本不符");
        }
    }
}

/// 向既有会话提交一条用户消息。
async fn submit_existing(storage: &impl Storage, session_id: nomic_core::SessionId, content: &str) {
    let acceptance = storage
        .record_submission(&Submission {
            request_id: RequestId::new(),
            target: SessionTarget::Existing(session_id),
            working_dir: std::path::PathBuf::new(),
            content: content.into(),
        })
        .await
        .unwrap();
    assert!(matches!(acceptance, Acceptance::Accepted { .. }));
}

#[tokio::test]
async fn interrupted_调用_恢复时部分输出回灌模型() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit_new(&storage, dir.path(), "任务").await;

    // 构造崩溃残留历史：助手请求调用，调用开始后被标记 interrupted
    let call = bash_call("cargo test");
    storage
        .append_step(
            session_id,
            Step::Message(Message::Assistant {
                id: nomic_core::MessageId::new(),
                text: String::new(),
                tool_calls: vec![call.clone()],
            }),
        )
        .await
        .unwrap();
    storage
        .append_step(session_id, Step::ToolCallStarted { call: call.clone() })
        .await
        .unwrap();
    storage
        .append_step(
            session_id,
            Step::ToolCallInterrupted {
                call_id: call.id,
                partial_output: Some("部分编译日志".into()),
            },
        )
        .await
        .unwrap();

    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["恢复继续"])]);
    let env = ScriptedEnv::inert();
    let (ran, _) = run_collect(&storage, &llm, &env).await;
    assert_eq!(ran.unwrap(), Some(session_id));

    // 回放：interrupted 调用以工具错误层回灌，部分输出可见
    let captured = llm.captured();
    match &captured[0].messages[..] {
        [
            Message::System { .. },
            Message::User { .. },
            Message::Assistant { tool_calls, .. },
            Message::Tool {
                call_id, result, ..
            },
        ] => {
            assert_eq!(tool_calls.as_slice(), std::slice::from_ref(&call));
            assert_eq!(*call_id, call.id);
            match result {
                ToolResult::Error { message } => {
                    assert!(message.contains("interrupted"));
                    assert!(message.contains("部分编译日志"));
                }
                other => panic!("interrupted 回灌必须为错误层: {other:?}"),
            }
        }
        other => panic!("恢复回放历史不符: {other:?}"),
    }
    // 恢复路径不得重新派发已中断的调用
    assert!(env.executions().is_empty());

    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn interrupted_调用_无部分输出时回灌中断说明() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit_new(&storage, dir.path(), "任务").await;

    // 崩溃残留历史：调用开始后被标记 interrupted，且无任何已提交输出
    let call = bash_call("cargo build");
    storage
        .append_step(
            session_id,
            Step::Message(Message::Assistant {
                id: nomic_core::MessageId::new(),
                text: String::new(),
                tool_calls: vec![call.clone()],
            }),
        )
        .await
        .unwrap();
    storage
        .append_step(session_id, Step::ToolCallStarted { call: call.clone() })
        .await
        .unwrap();
    storage
        .append_step(
            session_id,
            Step::ToolCallInterrupted {
                call_id: call.id,
                partial_output: None,
            },
        )
        .await
        .unwrap();

    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["恢复继续"])]);
    let env = ScriptedEnv::inert();
    let (ran, _) = run_collect(&storage, &llm, &env).await;
    assert_eq!(ran.unwrap(), Some(session_id));

    let captured = llm.captured();
    match &captured[0].messages[..] {
        [
            Message::System { .. },
            Message::User { .. },
            Message::Assistant { .. },
            Message::Tool { result, .. },
        ] => match result {
            ToolResult::Error { message } => {
                assert!(message.contains("interrupted"));
                assert!(
                    message.contains("no output committed"),
                    "无部分输出时必须回灌中断说明: {message}"
                );
            }
            other => panic!("interrupted 回灌必须为错误层: {other:?}"),
        },
        other => panic!("恢复回放历史不符: {other:?}"),
    }
    // 恢复路径不得重新派发已中断的调用
    assert!(env.executions().is_empty());

    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 工具回合中_llm_失败_已落库历史保留_重驱续跑() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit_new(&storage, dir.path(), "任务").await;

    let call = read_call("a.txt");
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Play(vec![ScriptedEvent::ToolCall(call.clone())]),
        ScriptedTurn::Fail(nomic_core::LlmError::Transport("timeout".into())),
        ScriptedTurn::reply(["续跑完成"]),
    ]);
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("内容A"))], Vec::new());

    // 首轮：工具执行与回灌已落库，第二轮请求失败
    let (result, _) = run_collect(&storage, &llm, &env).await;
    assert!(matches!(
        result.unwrap_err(),
        AgentError::Llm(nomic_core::LlmError::Transport(_))
    ));
    let steps = storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 4, "工具回合步骤必须已 checkpoint: {steps:?}");
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Pending);

    // 重驱：回放含工具回灌的同一历史，续跑至静止
    let (ran, _) = run_collect(&storage, &llm, &env).await;
    assert_eq!(ran.unwrap(), Some(session_id));
    let captured = llm.captured();
    assert_eq!(captured.len(), 3);
    let tool_messages: Vec<&Message> = captured[2]
        .messages
        .iter()
        .filter(|message| message.role() == Role::Tool)
        .collect();
    assert_eq!(tool_messages.len(), 1);
    assert_eq!(tool_messages[0].text(), "内容A");
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 环境派发故障_闭合为_interrupted_重驱恢复() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit_new(&storage, dir.path(), "任务").await;

    let call = bash_call("cargo build");
    let llm = ScriptedLlm::new(vec![ScriptedTurn::Play(vec![ScriptedEvent::ToolCall(
        call.clone(),
    )])]);
    // 剧本只剩一次派发：重驱后任何误派发都会触发 fast-fail panic
    let env = ScriptedEnv::new(
        vec![Err(EnvironmentError::Spawn("进程无法拉起".into()))],
        Vec::new(),
    );

    // 首轮：派发失败向上传播，调用以 interrupted 闭合（状态不确定，
    // 无已提交输出）
    let (result, events) = run_collect(&storage, &llm, &env).await;
    assert!(matches!(
        result.unwrap_err(),
        AgentError::Environment(EnvironmentError::Spawn(_))
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolCallStarted { .. }))
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, Event::ToolCallFinished { .. }))
    );
    let steps = storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 4, "派发故障必须闭合 in-flight 调用: {steps:?}");
    assert!(matches!(
        steps.last(),
        Some(Step::ToolCallInterrupted {
            call_id,
            partial_output: None,
        }) if *call_id == call.id
    ));
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Pending);

    // 重驱：历史配对完整，恢复面打开——interrupted 说明回灌模型，
    // 会话续跑至静止
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["换个方式"])]);
    let (ran, _) = run_collect(&storage, &llm, &env).await;
    assert_eq!(ran.unwrap(), Some(session_id));
    let captured = llm.captured();
    match &captured[0].messages[..] {
        [
            Message::System { .. },
            Message::User { .. },
            Message::Assistant { tool_calls, .. },
            Message::Tool {
                call_id, result, ..
            },
        ] => {
            assert_eq!(tool_calls.as_slice(), std::slice::from_ref(&call));
            assert_eq!(*call_id, call.id);
            assert!(
                matches!(result, ToolResult::Error { message } if message.contains("no output committed")),
                "interrupted 回灌必须为错误层: {result:?}"
            );
        }
        other => panic!("重驱回放历史不符: {other:?}"),
    }
    let steps = storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 5, "续跑助手消息必须落库: {steps:?}");
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}
