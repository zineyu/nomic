//! rig 适配器等价性验收（S5 #40）：S3/S4 剧本经真实 rig 适配器重跑。
//!
//! 剧本编码为 OpenAI Chat SSE 帧，经 rig 官方 `SequencedHttpClient`
//! 送达——真实请求编码与流解码，不触网。断言与 S3/S4 验收相同：
//! 事件序列、落库步骤、请求历史与工具声明；另加 API key 不落盘断言。

#[path = "common/agent_seam.rs"]
mod agent_seam;

use std::path::Path;

use agent_seam::{ProtocolFixture, ScriptedEnv, ScriptedLlm, next_session_event, wait_for};
use nomic_ai::testing::{
    SCRIPTED_API_KEY, ScriptedEvent as RigEvent, ScriptedRig, ScriptedTurn as RigTurn,
};
use nomic_core::{
    Acceptance, Event, ExecutionEnvironment, LlmAdapter, Message, RequestId, Role, SessionId,
    SessionState, SessionTarget, Step, Storage, Submission, ToolName, ToolResult,
};
use nomic_server::agent::{DEFAULT_SYSTEM_PROMPT, assemble_system_prompt};
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

/// 等待会话静止（事件到达后收尾是异步的）。
async fn wait_idle<L, E>(fixture: &ProtocolFixture<L, E>, session: SessionId)
where
    L: LlmAdapter + 'static,
    E: ExecutionEnvironment + 'static,
{
    wait_for(|| async {
        fixture
            .storage
            .get_session(session)
            .await
            .unwrap()
            .is_some_and(|stored| stored.state() == SessionState::Idle)
    })
    .await;
}

/// 角色与文本序列：系统提示 id 每次运行新生成，跨适配器比较用角色+文本。
fn transcript(messages: &[Message]) -> Vec<(Role, String)> {
    messages
        .iter()
        .map(|message| (message.role(), message.text().to_owned()))
        .collect()
}

#[tokio::test]
async fn rig_适配器重跑单轮对话_事件序列与落库步骤一致() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedRig::new(vec![RigTurn::reply(["好的", "，", "马上处理"])]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "帮我补一个单测").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;

    // 事件序列：逐 token 增量（共享同一消息 id）+ 消息完成
    let first = next_session_event(&mut client).await;
    let message_id = match &first {
        Event::TokenDelta {
            message_id, delta, ..
        } => {
            assert_eq!(delta, "好的");
            *message_id
        }
        other => panic!("首事件必须是增量: {other:?}"),
    };
    for expected in ["，", "马上处理"] {
        match next_session_event(&mut client).await {
            Event::TokenDelta {
                message_id: got,
                delta,
                ..
            } => {
                assert_eq!(got, message_id, "同一回合的增量共享消息 id");
                assert_eq!(delta, expected);
            }
            other => panic!("必须是增量: {other:?}"),
        }
    }
    match next_session_event(&mut client).await {
        Event::MessageCompleted { message, .. } => {
            assert_eq!(message.id(), message_id);
            assert_eq!(message.text(), "好的，马上处理");
        }
        other => panic!("必须完成回复: {other:?}"),
    }

    wait_idle(&fixture, session_id).await;
    let steps = fixture.storage.steps(session_id).await.unwrap();
    match &steps[..] {
        [Step::Message(user), Step::Message(assistant)] => {
            assert_eq!(user.role(), Role::User);
            assert_eq!(assistant.text(), "好的，马上处理");
        }
        other => panic!("落库步骤不符: {other:?}"),
    }

    // wire 层：真实 Chat Completions 请求，密钥只出现在 Authorization 头
    let sent = fixture.llm.sent();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].url.ends_with("/chat/completions"),
        "实际 URL: {}",
        sent[0].url
    );
    assert_eq!(
        sent[0].authorization.as_deref(),
        Some(format!("Bearer {SCRIPTED_API_KEY}").as_str())
    );
    assert!(!sent[0].body.contains(SCRIPTED_API_KEY));
}

#[tokio::test]
async fn rig_适配器重跑工具回合_请求历史与工具声明一致() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedRig::new(vec![
        RigTurn::Reply(vec![RigEvent::tool_call(
            "call_provider",
            "read",
            serde_json::json!({"path": "note.txt"}),
        )]),
        RigTurn::reply(["内容已收到"]),
    ]);
    let specs = LocalEnvironment::new().tool_specs().to_vec();
    let env = ScriptedEnv::new(vec![Ok(ToolResult::ok("文件内容"))], specs.clone());
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "帮我看下 note.txt").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;

    // 助手（含调用）请求一次工具；调用落库后执行、回灌、续跑
    match next_session_event(&mut client).await {
        Event::MessageCompleted { message, .. } => {
            assert_eq!(message.tool_calls().len(), 1, "助手消息携带工具调用");
        }
        other => panic!("工具回合先完成助手消息: {other:?}"),
    }
    let call_id = match next_session_event(&mut client).await {
        Event::ToolCallStarted { call, .. } => {
            assert_eq!(call.tool.as_str(), "read");
            call.id
        }
        other => panic!("必须收到工具调用开始: {other:?}"),
    };
    match next_session_event(&mut client).await {
        Event::ToolCallFinished {
            call_id: got,
            result,
            ..
        } => {
            assert_eq!(got, call_id);
            assert_eq!(result, ToolResult::ok("文件内容"));
        }
        other => panic!("必须收到工具调用结束: {other:?}"),
    }
    match next_session_event(&mut client).await {
        Event::MessageCompleted { message, .. } => assert_eq!(message.text(), "文件内容"),
        other => panic!("必须回灌工具结果: {other:?}"),
    }
    match next_session_event(&mut client).await {
        Event::TokenDelta { delta, .. } => assert_eq!(delta, "内容已收到"),
        other => panic!("续跑必须先收到增量: {other:?}"),
    }

    wait_idle(&fixture, session_id).await;

    // 领域请求快照：工具声明与派发环境一致；第二轮历史回放含工具结果
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 2);
    assert_eq!(captured[0].tools, specs);
    let second = transcript(&captured[1].messages);
    let assistant = captured[1]
        .messages
        .iter()
        .find(|message| message.role() == Role::Assistant && !message.tool_calls().is_empty())
        .expect("第二轮历史含带工具调用的助手消息");
    assert_eq!(assistant.tool_calls()[0].id, call_id);
    let tool = captured[1]
        .messages
        .iter()
        .find(|message| matches!(message, Message::Tool { .. }))
        .expect("第二轮历史含工具结果消息");
    assert_eq!(tool.text(), "文件内容");
    assert_eq!(
        second.first().map(|(role, _)| *role),
        Some(Role::System),
        "历史以系统提示开头"
    );

    // wire 层：调用 id 与结果配对一致
    let sent = fixture.llm.sent();
    assert_eq!(sent.len(), 2);
    let body: serde_json::Value = serde_json::from_str(&sent[1].body).unwrap();
    let messages = body["messages"].as_array().unwrap();
    let call = call_id.to_string();
    assert!(
        messages.iter().any(|message| {
            message["tool_calls"]
                .as_array()
                .is_some_and(|calls| calls.iter().any(|c| c["id"] == call.as_str()))
        }),
        "wire 上助手调用 id 与领域 id 一致"
    );
    assert!(
        messages.iter().any(|message| {
            message["role"] == "tool" && message["tool_call_id"] == call.as_str()
        }),
        "wire 上工具结果与调用配对"
    );
    assert!(
        body["tools"]
            .as_array()
            .is_some_and(|tools| tools.len() == specs.len()),
        "wire 上声明全部工具"
    );
}

#[tokio::test]
async fn rig_与脚本替身的请求序列逻辑等价() {
    let rig_dir = tempfile::tempdir().unwrap();
    let rig = ScriptedRig::new(vec![RigTurn::reply(["好的"])]);
    let rig_fixture = ProtocolFixture::start(rig_dir.path(), rig, ScriptedEnv::inert()).await;
    let mut client = rig_fixture.connect().await;
    let session = submit_new(&mut client, rig_dir.path(), "任务").await;
    wait_idle(&rig_fixture, session).await;

    let scripted_dir = tempfile::tempdir().unwrap();
    let scripted = ScriptedLlm::new(vec![agent_seam::ScriptedTurn::reply(["好的"])]);
    let scripted_fixture =
        ProtocolFixture::start(scripted_dir.path(), scripted, ScriptedEnv::inert()).await;
    let mut client = scripted_fixture.connect().await;
    let session = submit_new(&mut client, scripted_dir.path(), "任务").await;
    wait_idle(&scripted_fixture, session).await;

    let rig_captured = rig_fixture.llm.captured();
    let scripted_captured = scripted_fixture.llm.captured();
    assert_eq!(
        transcript(&rig_captured[0].messages),
        transcript(&scripted_captured[0].messages),
        "两种适配器的请求消息序列等价"
    );
    assert_eq!(
        rig_captured[0].tools, scripted_captured[0].tools,
        "两种适配器的工具声明等价"
    );
}

#[tokio::test]
async fn rig_传输失败_会话挂回待处理且不落助手消息() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedRig::new(vec![RigTurn::TransportFailure("connection reset".into())]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "任务").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    match next_session_event(&mut client).await {
        Event::Error { kind, message, .. } => {
            assert_eq!(kind, nomic_core::EventErrorKind::Llm);
            assert!(message.contains("connection reset"), "实际: {message}");
        }
        other => panic!("传输失败必须发出错误事件: {other:?}"),
    }

    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 1, "失败不得追加助手步骤: {steps:?}");
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Pending);
}

#[tokio::test]
async fn api_key_不进入任何落盘内容() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedRig::new(vec![RigTurn::reply(["完成"])]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "任务").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    match next_session_event(&mut client).await {
        Event::TokenDelta { .. } => {}
        other => panic!("必须收到增量: {other:?}"),
    }
    wait_idle(&fixture, session_id).await;

    // 密钥确实被使用：出现在 Authorization 头，且不在请求体
    let sent = fixture.llm.sent();
    assert!(
        sent[0]
            .authorization
            .as_deref()
            .is_some_and(|value| value.contains(SCRIPTED_API_KEY))
    );
    assert!(!sent[0].body.contains(SCRIPTED_API_KEY));

    // 落盘内容（SQLite 文件）不得含密钥
    let db = std::fs::read(dir.path().join("nomic.db")).unwrap();
    let text = String::from_utf8_lossy(&db);
    assert!(
        !text.contains(SCRIPTED_API_KEY),
        "SQLite 落盘内容含 API key"
    );
}

#[tokio::test]
async fn rig_适配器重跑系统提示注入_首消息等于组装提示() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "项目规约：禁止 unwrap").unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "回答使用中文").unwrap();
    let llm = ScriptedRig::new(vec![RigTurn::reply(["收到"])]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session = submit_new(&mut client, dir.path(), "继续").await;
    wait_idle(&fixture, session).await;

    let Message::System { text, .. } = &fixture.llm.captured()[0].messages[0] else {
        panic!("请求历史首条必须是系统提示");
    };
    assert_eq!(text, &assemble_system_prompt(dir.path()).unwrap());
    assert!(text.starts_with(DEFAULT_SYSTEM_PROMPT.trim_end()));
    assert!(text.contains("## AGENTS.md\n\n项目规约：禁止 unwrap"));
    assert!(text.contains("## CLAUDE.md\n\n回答使用中文"));
    assert!(
        text.find("## AGENTS.md").unwrap() < text.find("## CLAUDE.md").unwrap(),
        "AGENTS.md 节必须先于 CLAUDE.md 节"
    );
    for spec in LocalEnvironment::new().tool_specs() {
        assert!(
            !text.contains(spec.name.as_str()),
            "系统提示不得出现工具名 {}",
            spec.name
        );
    }
}

#[tokio::test]
async fn rig_适配器重跑三层结果回灌() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedRig::new(vec![
        RigTurn::Reply(vec![
            RigEvent::tool_call("c1", "read", serde_json::json!({"path": "a.txt"})),
            RigEvent::tool_call("c2", "read", serde_json::json!({"path": "b.txt"})),
            RigEvent::tool_call("c3", "read", serde_json::json!({"path": "c.txt"})),
        ]),
        RigTurn::reply(["完成"]),
    ]);
    let env = ScriptedEnv::new(
        vec![
            Ok(ToolResult::ok("标准输出")),
            Ok(ToolResult::error("文件不存在")),
            Ok(ToolResult::rejected("结构化拒绝原因：命令被策略拒绝")),
        ],
        LocalEnvironment::new().tool_specs().to_vec(),
    );
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "跑三个工具").await;
    wait_idle(&fixture, session_id).await;

    let steps = fixture.storage.steps(session_id).await.unwrap();
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
    let captured = fixture.llm.captured();
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

    // wire 层：三条工具结果与三个调用 id 一一配对
    let body: serde_json::Value =
        serde_json::from_str(fixture.llm.sent()[1].body.as_str()).unwrap();
    let messages = body["messages"].as_array().unwrap();
    let call_ids: Vec<&str> = messages
        .iter()
        .filter_map(|message| message["tool_calls"].as_array())
        .flatten()
        .filter_map(|call| call["id"].as_str())
        .collect();
    let result_ids: Vec<&str> = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .filter_map(|message| message["tool_call_id"].as_str())
        .collect();
    assert_eq!(call_ids, result_ids, "wire 上调用与结果 id 配对");
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| message["content"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        ["标准输出", "文件不存在", "结构化拒绝原因：命令被策略拒绝"]
    );
}

#[tokio::test]
async fn rig_适配器重跑空回复_空助手消息落库且静止() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedRig::new(vec![RigTurn::Reply(Vec::new())]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "任务").await;
    wait_idle(&fixture, session_id).await;

    let steps = fixture.storage.steps(session_id).await.unwrap();
    match &steps[..] {
        [Step::Message(user), Step::Message(assistant)] => {
            assert_eq!(user.role(), Role::User);
            assert_eq!(assistant.text(), "");
            assert!(assistant.tool_calls().is_empty());
        }
        other => panic!("空回复必须落库空助手消息: {other:?}"),
    }
}

#[tokio::test]
async fn rig_适配器重跑未注册工具_派发失败挂回待处理() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedRig::new(vec![RigTurn::Reply(vec![RigEvent::tool_call(
        "c1",
        "nope",
        serde_json::json!({}),
    )])]);
    let env = ScriptedEnv::new(
        vec![Err(nomic_core::EnvironmentError::UnknownTool(
            ToolName::new("nope"),
        ))],
        Vec::new(),
    );
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "任务").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    // 助手消息完成 → 调用开始 → 派发错误
    match next_session_event(&mut client).await {
        Event::MessageCompleted { message, .. } => assert_eq!(message.tool_calls().len(), 1),
        other => panic!("必须先完成助手消息: {other:?}"),
    }
    match next_session_event(&mut client).await {
        Event::ToolCallStarted { call, .. } => assert_eq!(call.tool.as_str(), "nope"),
        other => panic!("必须收到调用开始: {other:?}"),
    }
    match next_session_event(&mut client).await {
        Event::Error { kind, message, .. } => {
            assert_eq!(kind, nomic_core::EventErrorKind::Environment);
            assert!(
                message.contains("unknown tool requested"),
                "实际: {message}"
            );
        }
        other => panic!("末事件必须是派发错误: {other:?}"),
    }

    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 4, "派发故障必须闭合 in-flight 调用: {steps:?}");
    assert!(matches!(
        steps.last(),
        Some(Step::ToolCallInterrupted { .. })
    ));
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Pending);
}

#[tokio::test]
async fn rig_适配器重跑派发故障_闭合_interrupted_重驱恢复() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedRig::new(vec![
        RigTurn::Reply(vec![RigEvent::tool_call(
            "c1",
            "read",
            serde_json::json!({"path": "a.txt"}),
        )]),
        RigTurn::reply(["换个方式"]),
    ]);
    let env = ScriptedEnv::new(
        vec![Err(nomic_core::EnvironmentError::Spawn(
            "进程无法拉起".into(),
        ))],
        Vec::new(),
    );
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "任务").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    // 助手消息完成 → 调用开始 → 派发错误
    let _ = next_session_event(&mut client).await;
    let _ = next_session_event(&mut client).await;
    match next_session_event(&mut client).await {
        Event::Error { kind, message, .. } => {
            assert_eq!(kind, nomic_core::EventErrorKind::Environment);
            assert!(message.contains("failed to spawn"), "实际: {message}");
        }
        other => panic!("末事件必须是派发故障: {other:?}"),
    }
    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert!(matches!(
        steps.last(),
        Some(Step::ToolCallInterrupted {
            partial_output: None,
            ..
        })
    ));
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Pending);

    // 重驱：interrupted 说明回灌模型，会话续跑至静止
    client.resume(session_id).await.unwrap();
    wait_idle(&fixture, session_id).await;

    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 2);
    let tool_message = captured[1]
        .messages
        .iter()
        .find(|message| matches!(message, Message::Tool { .. }))
        .expect("重驱回放必须含 interrupted 回灌");
    assert!(
        matches!(
            tool_message,
            Message::Tool { result: ToolResult::Error { message }, .. }
                if message.contains("no output committed")
        ),
        "interrupted 回灌必须为错误层: {tool_message:?}"
    );
    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 5, "续跑助手消息必须落库: {steps:?}");
}
