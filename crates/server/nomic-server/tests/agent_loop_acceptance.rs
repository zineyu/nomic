//! agent loop seam 验收（#37，S7 起协议驱动）：剧本驱动闭环、系统提示
//! 组装、串行释放与失败路径。
//!
//! 测试经进程内 server 实例（真实 Unix socket）+ 协议客户端驱动；
//! 存储与替身快照断言不变。时序敏感断言经闸门钉住：订阅先于
//! 事件到达、落库先于完成事件。

#[path = "common/agent_seam.rs"]
mod agent_seam;

use std::path::Path;
use std::sync::Arc;

use agent_seam::{
    ProtocolFixture, ScriptedEnv, ScriptedEvent, ScriptedLlm, ScriptedTurn, next_session_event,
    wait_for,
};
use nomic_core::{
    Acceptance, Event, Message, MessageId, RequestId, Role, SessionId, SessionState, SessionTarget,
    Step, Storage, Submission, ToolCall, ToolCallId, ToolName,
};
use nomic_server::agent::{DEFAULT_SYSTEM_PROMPT, Worker};

/// 提交一条用户消息到新会话，返回受理的会话 id。
async fn submit(client: &mut nomic_server::client::Client, dir: &Path, content: &str) -> SessionId {
    let submission = Submission {
        request_id: RequestId::new(),
        target: SessionTarget::New,
        working_dir: dir.to_path_buf(),
        content: content.into(),
    };
    match client.submit(&submission).await.unwrap() {
        Acceptance::Accepted { session_id } => session_id,
        Acceptance::Duplicate { .. } => panic!("首次提交不应判重"),
    }
}

/// 等待会话回到静止（事件到达后收尾是异步的）。
async fn wait_idle(fixture: &ProtocolFixture<ScriptedLlm, ScriptedEnv>, session: SessionId) {
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

#[tokio::test]
async fn 剧本驱动完成单轮对话_事件序列与落库步骤一一对应() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::PlayGated(
        gate.clone(),
        vec![
            ScriptedEvent::Token("好的".into()),
            ScriptedEvent::Token("，".into()),
            ScriptedEvent::Token("马上处理".into()),
        ],
    )]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit(&mut client, dir.path(), "帮我补一个单测").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    // 3 个 token + 流终止各等待一次放行
    gate.release_all(4);

    // 事件序列：逐 token 增量（共享同一消息 id）+ 消息完成
    let events = Vec::from([
        next_session_event(&mut client).await,
        next_session_event(&mut client).await,
        next_session_event(&mut client).await,
        next_session_event(&mut client).await,
    ]);
    let completed = match &events[..] {
        [
            Event::TokenDelta {
                session_id: s1,
                message_id: _,
                delta: d1,
            },
            Event::TokenDelta { delta: d2, .. },
            Event::TokenDelta { delta: d3, .. },
            Event::MessageCompleted {
                session_id: s2,
                message,
            },
        ] if *s1 == session_id && *s2 == session_id => {
            assert_eq!([d1.as_str(), d2, d3], ["好的", "，", "马上处理"]);
            message
        }
        other => panic!("事件序列与剧本不符: {other:?}"),
    };

    // 落库步骤与完成事件一一对应：用户消息先于助手消息
    let steps = fixture.storage.steps(session_id).await.unwrap();
    match &steps[..] {
        [Step::Message(user), Step::Message(assistant)] => {
            assert_eq!(user.role(), Role::User);
            assert_eq!(user.text(), "帮我补一个单测");
            assert_eq!(assistant.role(), Role::Assistant);
            assert_eq!(assistant.text(), "好的，马上处理");
            assert_eq!(assistant.id(), completed.id());
        }
        other => panic!("落库步骤与事件不符: {other:?}"),
    }

    // 请求历史：系统提示开头 + 用户消息；无工具注册
    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].session, session_id);
    assert!(captured[0].tools.is_empty());
    match &captured[0].messages[..] {
        [
            Message::System { text, .. },
            Message::User { text: content, .. },
        ] => {
            assert_eq!(text, DEFAULT_SYSTEM_PROMPT.trim_end());
            assert_eq!(content, "帮我补一个单测");
        }
        other => panic!("请求历史不符: {other:?}"),
    }

    // 释放：会话回到静止，无待处理工作
    wait_idle(&fixture, session_id).await;
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Idle);
    assert!(
        fixture
            .storage
            .session_ids_in_state(SessionState::Pending)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn 系统提示等于内置默认加存在的上下文文件() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "项目规约：禁止 unwrap").unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "回答使用中文").unwrap();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["收到"])]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit(&mut client, dir.path(), "继续").await;

    wait_for(|| async {
        fixture
            .storage
            .get_session(session_id)
            .await
            .unwrap()
            .is_some_and(|session| session.state() == SessionState::Idle)
    })
    .await;

    let captured = fixture.llm.captured();
    let Message::System { text, .. } = &captured[0].messages[0] else {
        panic!("请求历史首条必须是系统提示: {:?}", captured[0].messages);
    };
    assert!(text.starts_with(DEFAULT_SYSTEM_PROMPT.trim_end()));
    assert!(text.contains("## AGENTS.md\n\n项目规约：禁止 unwrap"));
    assert!(text.contains("## CLAUDE.md\n\n回答使用中文"));
    assert!(
        text.find("## AGENTS.md").unwrap() < text.find("## CLAUDE.md").unwrap(),
        "AGENTS.md 节必须先于 CLAUDE.md 节"
    );
    wait_idle(&fixture, session_id).await;
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 串行领取_每次一个会话_先受理先执行_静止后释放() {
    let dir = tempfile::tempdir().unwrap();
    let first_gate = agent_seam::gate();
    let second_gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::PlayGated(first_gate.clone(), vec![ScriptedEvent::Token("一".into())]),
        ScriptedTurn::PlayGated(second_gate.clone(), vec![ScriptedEvent::Token("二".into())]),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let first = submit(&mut client, dir.path(), "任务一").await;
    let second = submit(&mut client, dir.path(), "任务二").await;
    assert_ne!(first, second);

    // 两会话同时待处理：首会话已被领取（Running），次会话仍待处理
    wait_for(|| async {
        let first_state = fixture
            .storage
            .get_session(first)
            .await
            .unwrap()
            .map(|session| session.state());
        let second_state = fixture
            .storage
            .get_session(second)
            .await
            .unwrap()
            .map(|session| session.state());
        first_state == Some(SessionState::Running) && second_state == Some(SessionState::Pending)
    })
    .await;

    // 首轮：每次运行只推进一个会话；闸门钉住「订阅先于事件」
    agent_seam::subscribe_synced(&mut client, first).await;
    first_gate.release_all(2);
    let first_events = Vec::from([
        next_session_event(&mut client).await,
        next_session_event(&mut client).await,
    ]);
    assert!(
        first_events.iter().all(|event| matches!(
            event,
            Event::TokenDelta { session_id, .. } | Event::MessageCompleted { session_id, .. }
            if *session_id == first
        )),
        "首轮事件必须全部属于第一个会话: {first_events:?}"
    );
    wait_for(|| async {
        fixture
            .storage
            .get_session(first)
            .await
            .unwrap()
            .is_some_and(|session| session.state() == SessionState::Idle)
    })
    .await;

    // 次轮：第二个会话按受理顺序推进（闸门同样钉住订阅先于事件）
    agent_seam::subscribe_synced(&mut client, second).await;
    second_gate.release_all(2);
    let second_events = Vec::from([
        next_session_event(&mut client).await,
        next_session_event(&mut client).await,
    ]);
    assert!(
        second_events.iter().all(|event| matches!(
            event,
            Event::TokenDelta { session_id, .. } | Event::MessageCompleted { session_id, .. }
            if *session_id == second
        )),
        "次轮事件必须全部属于第二个会话: {second_events:?}"
    );

    // 串行隔离：两会话各自落库，互不可见
    for (session, reply) in [(first, "一"), (second, "二")] {
        let steps = fixture.storage.steps(session).await.unwrap();
        match &steps[..] {
            [Step::Message(user), Step::Message(assistant)] => {
                assert_eq!(assistant.text(), reply);
                assert_eq!(user.role(), Role::User);
            }
            other => panic!("会话 {session} 落库步骤不符: {other:?}"),
        }
        wait_idle(&fixture, session).await;
        let session = fixture.storage.get_session(session).await.unwrap().unwrap();
        assert_eq!(session.state(), SessionState::Idle);
    }

    // 队列耗尽：worker 无事可做
    assert!(
        fixture
            .storage
            .session_ids_in_state(SessionState::Pending)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn 事件增量与完成携带的助手消息_id_与落库步骤一致() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::PlayGated(
        gate.clone(),
        vec![ScriptedEvent::Token("x".into())],
    )]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit(&mut client, dir.path(), "id 一致性").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    gate.release_all(2);
    let events = Vec::from([
        next_session_event(&mut client).await,
        next_session_event(&mut client).await,
    ]);

    // 落库后重新读取，id 与流式期间事件携带的 id 相同
    let steps = fixture.storage.steps(session_id).await.unwrap();
    let Step::Message(Message::Assistant { id, .. }) = &steps[1] else {
        panic!("第二步必须是助手消息: {steps:?}");
    };
    let streamed_ids: Vec<MessageId> = events
        .iter()
        .filter_map(|event| match event {
            Event::TokenDelta { message_id, .. } => Some(*message_id),
            _ => None,
        })
        .collect();
    assert_eq!(streamed_ids, vec![*id]);
    match &events[1] {
        Event::MessageCompleted { message, .. } => assert_eq!(message.id(), *id),
        other => panic!("末事件必须是消息完成: {other:?}"),
    }
}

#[tokio::test]
async fn llm_传输错误_会话挂回待处理_重驱按同一历史续跑() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::GatedFail(
            gate.clone(),
            nomic_core::LlmError::Transport("timeout".into()),
        ),
        ScriptedTurn::reply(["重试成功"]),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit(&mut client, dir.path(), "任务").await;

    // 首轮：订阅后放行，错误经事件流到达
    agent_seam::subscribe_synced(&mut client, session_id).await;
    gate.release();
    match next_session_event(&mut client).await {
        Event::Error {
            session_id: got,
            kind,
            message,
        } => {
            assert_eq!(got, session_id);
            assert_eq!(kind, nomic_core::EventErrorKind::Llm);
            assert!(
                message.contains("transport error: timeout"),
                "实际: {message}"
            );
        }
        other => panic!("首轮必须收到错误事件: {other:?}"),
    }

    // 失败悬挂保护：会话回到待处理，历史未推进
    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 1, "失败不得追加助手步骤: {steps:?}");
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Pending);

    // 重驱：Resume 回放历史并触发调度；LLM 收到与首次完全一致的
    // 历史序列（系统提示 + 用户消息）
    let replayed = client.resume(session_id).await.unwrap();
    assert_eq!(replayed.len(), 1, "回放只含用户消息: {replayed:?}");
    assert_eq!(replayed[0].role(), Role::User);
    match next_session_event(&mut client).await {
        Event::TokenDelta { delta, .. } => assert_eq!(delta, "重试成功"),
        other => panic!("重驱必须先收到增量: {other:?}"),
    }
    match next_session_event(&mut client).await {
        Event::MessageCompleted { message, .. } => assert_eq!(message.text(), "重试成功"),
        other => panic!("重驱必须完成回复: {other:?}"),
    }

    let captured = fixture.llm.captured();
    assert_eq!(captured.len(), 2);
    // 系统提示 id 是每次运行新生成的临时值（系统提示不落库，
    // 运行时确定性重建）；一致性断言针对角色与文本序列
    assert_ne!(
        captured[0].messages[0].id(),
        captured[1].messages[0].id(),
        "系统消息 id 必须为每次运行新生成的临时值"
    );
    let transcript = |messages: &[Message]| {
        messages
            .iter()
            .map(|message| (message.role(), message.text().to_string()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        transcript(&captured[0].messages),
        transcript(&captured[1].messages),
        "重驱必须回放同一请求序列"
    );

    // 续跑完成：助手消息落库、会话静止
    let steps = fixture.storage.steps(session_id).await.unwrap();
    match &steps[..] {
        [Step::Message(user), Step::Message(assistant)] => {
            assert_eq!(user.role(), Role::User);
            assert_eq!(assistant.text(), "重试成功");
        }
        other => panic!("落库步骤不符: {other:?}"),
    }
    wait_idle(&fixture, session_id).await;
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 请求未注册工具名_派发失败挂回待处理() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let call = ToolCall {
        id: ToolCallId::new(),
        tool: ToolName::new("nope"),
        arguments: serde_json::json!({}),
    };
    let llm = ScriptedLlm::new(vec![ScriptedTurn::PlayGated(
        gate.clone(),
        vec![ScriptedEvent::ToolCall(call.clone())],
    )]);
    // 执行环境返回未注册名称的显式失败（真实注册表路径见
    // local_tools_acceptance）
    let env = ScriptedEnv::new(
        vec![Err(nomic_core::EnvironmentError::UnknownTool(
            ToolName::new("nope"),
        ))],
        Vec::new(),
    );
    let fixture = ProtocolFixture::start(dir.path(), llm, env).await;
    let mut client = fixture.connect().await;
    let session_id = submit(&mut client, dir.path(), "任务").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    // 调用请求事件 + 流终止各等待一次放行
    gate.release_all(2);
    let events = Vec::from([
        next_session_event(&mut client).await,
        next_session_event(&mut client).await,
        next_session_event(&mut client).await,
    ]);

    // 助手消息与调用开始步骤已 checkpoint，完成事件未发出
    assert!(events.iter().any(|event| matches!(
        event,
        Event::MessageCompleted { message, .. } if message.tool_calls().len() == 1
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ToolCallStarted { call: started, .. } if *started == call
    )));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, Event::ToolCallFinished { .. }))
    );
    match events.last() {
        Some(Event::Error { kind, message, .. }) => {
            assert_eq!(*kind, nomic_core::EventErrorKind::Environment);
            assert!(
                message.contains("unknown tool requested"),
                "实际: {message}"
            );
        }
        other => panic!("末事件必须是派发错误: {other:?}"),
    }
    // 派发快照确认调用到达执行环境（与 tool_turn_acceptance 共用替身项）
    assert_eq!(fixture.env.executions().len(), 1);

    // 会话回到待处理：统一失败悬挂语义；本类为确定性协议违约，
    // 不属于状态不确定的崩溃恢复，可重试类别区分由 S10 决定。
    // 派发失败一律以 interrupted 闭合历史，保持回放配对完整
    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert_eq!(
        steps.len(),
        4,
        "助手步骤、开始步骤与 interrupted 闭合必须已落库: {steps:?}"
    );
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
async fn 助手消息先落库_完成事件后才释放() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::PlayGated(
        gate.clone(),
        vec![
            ScriptedEvent::Token("回".into()),
            ScriptedEvent::Token("复".into()),
        ],
    )]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit(&mut client, dir.path(), "顺序").await;
    agent_seam::subscribe_synced(&mut client, session_id).await;

    // 事件与落库的先后经闸门钉住：放行前 worker 阻塞在事件流上，
    // 存储回读与事件到达严格交替
    let persisted = |expected: usize| {
        let storage = Arc::clone(&fixture.storage);
        async move { storage.steps(session_id).await.unwrap().len() == expected }
    };
    assert!(
        persisted(1).await,
        "放行前只有用户消息落库（worker 阻塞在事件流上）"
    );
    gate.release();
    match next_session_event(&mut client).await {
        Event::TokenDelta { delta, .. } => assert_eq!(delta, "回"),
        other => panic!("首事件必须是增量: {other:?}"),
    }
    assert!(
        persisted(1).await,
        "增量事件先于落库发出：助手步骤不得提前落库"
    );
    gate.release();
    match next_session_event(&mut client).await {
        Event::TokenDelta { delta, .. } => assert_eq!(delta, "复"),
        other => panic!("次事件必须是增量: {other:?}"),
    }
    // worker 此刻阻塞在流终止闸门：助手步骤仍不得落库
    assert!(
        persisted(1).await,
        "增量事件先于落库发出：助手步骤不得提前落库"
    );
    gate.release();
    match next_session_event(&mut client).await {
        Event::MessageCompleted { message, .. } => assert_eq!(message.text(), "回复"),
        other => panic!("末事件必须是消息完成: {other:?}"),
    }
    // 完成事件到达时助手消息必须已落库（checkpoint 先行；事件经
    // hub 转发到本连接必然晚于落库与发出）
    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert!(
        matches!(
            steps.last(),
            Some(Step::Message(Message::Assistant { text, .. })) if text == "回复"
        ),
        "完成事件发出前助手消息必须已落库: {steps:?}"
    );
    wait_idle(&fixture, session_id).await;
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 空token剧本_助手消息为空文本_会话仍静止() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::PlayGated(gate.clone(), Vec::new())]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit(&mut client, dir.path(), "空回复").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    // 空剧本：流终止仍需一次放行
    gate.release();

    // 无增量事件，直接完成；空文本也构成完整助手消息
    match next_session_event(&mut client).await {
        Event::MessageCompleted { message, .. } => {
            assert_eq!(message.role(), Role::Assistant);
            assert_eq!(message.text(), "");
        }
        other => panic!("空剧本只应产出完成事件: {other:?}"),
    }
    let steps = fixture.storage.steps(session_id).await.unwrap();
    match &steps[..] {
        [Step::Message(user), Step::Message(assistant)] => {
            assert_eq!(user.role(), Role::User);
            assert_eq!(assistant.text(), "");
        }
        other => panic!("落库步骤不符: {other:?}"),
    }
    wait_idle(&fixture, session_id).await;
    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 上下文路径为目录_读取失败向上传播且挂回待处理() {
    let dir = tempfile::tempdir().unwrap();
    // 与文件同名的目录：读取失败（非 NotFound）必须传播，不得静默跳过
    std::fs::create_dir(dir.path().join("AGENTS.md")).unwrap();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::reply(["不应到达"])]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit(&mut client, dir.path(), "任务").await;

    // 提示组装在 LLM 请求之前，无闸门可钉；订阅在先，首轮失败的
    // 错误事件必达（提示组装每次运行都失败，阻塞读取确定）；
    // 随后 Resume 确定性重驱，重驱后的错误事件同样必达
    agent_seam::subscribe_synced(&mut client, session_id).await;
    for round in 0..2 {
        match next_session_event(&mut client).await {
            Event::Error { kind, message, .. } => {
                assert_eq!(kind, nomic_core::EventErrorKind::Internal);
                assert!(
                    message.contains("system prompt context unreadable"),
                    "实际: {message}"
                );
            }
            other => panic!("必须收到提示组装失败错误: {other:?}"),
        }
        if round == 0 {
            let replayed = client.resume(session_id).await.unwrap();
            assert_eq!(replayed.len(), 1);
        }
    }
    assert!(
        fixture.llm.captured().is_empty(),
        "提示组装失败不得请求 LLM"
    );

    let session = fixture
        .storage
        .get_session(session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.state(), SessionState::Pending);
}

#[tokio::test]
async fn 非待处理会话_幂等跳过且不触碰替身() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let _client = fixture.connect().await;

    // 直接创建（Idle）会话：调度请求对它幂等跳过
    let session = fixture
        .storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    let mut events = Vec::new();
    let ran = Worker::new(&*fixture.storage, &*fixture.llm, &*fixture.env)
        .run_session(session.id(), &mut |event| events.push(event))
        .await;
    assert!(!ran.unwrap(), "非待处理会话必须幂等跳过");
    assert!(events.is_empty());
    assert!(fixture.llm.captured().is_empty());
}
