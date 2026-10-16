//! agent loop seam 验收（#37）：剧本驱动闭环、系统提示组装、串行释放与失败路径。

#[path = "common/agent_seam.rs"]
mod agent_seam;

mod common;

use std::path::Path;

use agent_seam::{ScriptedLlm, ScriptedTurn, run_collect};
use nomic_core::{
    Acceptance, Event, LlmError, Message, MessageId, RequestId, Role, SessionId, SessionState,
    SessionTarget, Step, Storage, Submission,
};
use nomic_server::agent::{AgentError, DEFAULT_SYSTEM_PROMPT, Worker};

use crate::common::open_storage;

/// 提交一条用户消息到新会话，返回受理的会话 id。
async fn submit(storage: &impl Storage, dir: &Path, content: &str) -> SessionId {
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

#[tokio::test]
async fn 剧本驱动完成单轮对话_事件序列与落库步骤一一对应() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit(&storage, dir.path(), "帮我补一个单测").await;

    let llm = ScriptedLlm::new(vec![ScriptedTurn::Reply(vec![
        "好的".into(),
        "，".into(),
        "马上处理".into(),
    ])]);
    let (ran, events) = run_collect(&storage, &llm).await;
    assert_eq!(ran.unwrap(), Some(session_id));

    // 事件序列：逐 token 增量（共享同一消息 id）+ 消息完成
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
    let steps = storage.steps(session_id).await.unwrap();
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
    let captured = llm.captured();
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

    // 释放：会话回到静止，待处理队列已空
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
    assert_eq!(storage.claim_next_pending().await.unwrap(), None);
}

#[tokio::test]
async fn 系统提示等于内置默认加存在的上下文文件() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AGENTS.md"), "项目规约：禁止 unwrap").unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "回答使用中文").unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit(&storage, dir.path(), "继续").await;

    let llm = ScriptedLlm::new(vec![ScriptedTurn::Reply(vec!["收到".into()])]);
    let (ran, _) = run_collect(&storage, &llm).await;
    assert_eq!(ran.unwrap(), Some(session_id));

    let captured = llm.captured();
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
}

#[tokio::test]
async fn 串行领取_每次一个会话_先受理先执行_静止后释放() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let first = submit(&storage, dir.path(), "任务一").await;
    let second = submit(&storage, dir.path(), "任务二").await;
    assert_ne!(first, second);

    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Reply(vec!["一".into()]),
        ScriptedTurn::Reply(vec!["二".into()]),
    ]);

    // 每次运行只推进一个会话，按受理顺序
    let (first_ran, first_events) = run_collect(&storage, &llm).await;
    assert_eq!(first_ran.unwrap(), Some(first));
    assert!(
        first_events.iter().all(|event| matches!(
            event,
            Event::TokenDelta { session_id, .. } | Event::MessageCompleted { session_id, .. }
            if *session_id == first
        )),
        "首轮事件必须全部属于第一个会话: {first_events:?}"
    );
    let (second_ran, second_events) = run_collect(&storage, &llm).await;
    assert_eq!(second_ran.unwrap(), Some(second));
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
        let steps = storage.steps(session).await.unwrap();
        match &steps[..] {
            [Step::Message(user), Step::Message(assistant)] => {
                assert_eq!(assistant.text(), reply);
                assert_eq!(user.role(), Role::User);
            }
            other => panic!("会话 {session} 落库步骤不符: {other:?}"),
        }
        let session = storage.get_session(session).await.unwrap().unwrap();
        assert_eq!(session.state(), SessionState::Idle);
    }

    // 队列耗尽：worker 无事可做
    let (idle_ran, idle_events) = run_collect(&storage, &llm).await;
    assert_eq!(idle_ran.unwrap(), None);
    assert!(idle_events.is_empty());
}

#[tokio::test]
async fn 事件增量与完成携带的助手消息_id_与落库步骤一致() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit(&storage, dir.path(), "id 一致性").await;

    let llm = ScriptedLlm::new(vec![ScriptedTurn::Reply(vec!["x".into()])]);
    let (_, events) = run_collect(&storage, &llm).await;

    // 落库后重新读取，id 与流式期间事件携带的 id 相同
    let steps = storage.steps(session_id).await.unwrap();
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
    let storage = open_storage(dir.path());
    let session_id = submit(&storage, dir.path(), "任务").await;

    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Fail(LlmError::Transport("timeout".into())),
        ScriptedTurn::Reply(vec!["重试成功".into()]),
    ]);

    // 首次运行：错误向上传播，不推进任何状态
    let (result, events) = run_collect(&storage, &llm).await;
    assert!(matches!(
        result.unwrap_err(),
        AgentError::Llm(LlmError::Transport(_))
    ));
    assert!(events.is_empty(), "失败前不得发出事件: {events:?}");
    let steps = storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 1, "失败不得追加助手步骤: {steps:?}");

    // 失败悬挂保护：会话回到待处理，可被调度器重驱
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Pending);

    // 重驱：LLM 收到与首次完全一致的历史序列（系统提示 + 用户消息）
    let (result, _) = run_collect(&storage, &llm).await;
    assert_eq!(result.unwrap(), Some(session_id));
    let captured = llm.captured();
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
    let steps = storage.steps(session_id).await.unwrap();
    match &steps[..] {
        [Step::Message(user), Step::Message(assistant)] => {
            assert_eq!(user.role(), Role::User);
            assert_eq!(assistant.text(), "重试成功");
        }
        other => panic!("落库步骤不符: {other:?}"),
    }
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 未注册工具却请求工具调用_协议违约挂回待处理() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit(&storage, dir.path(), "任务").await;

    let llm = ScriptedLlm::new(vec![ScriptedTurn::RequestToolCall]);
    let (result, events) = run_collect(&storage, &llm).await;
    assert!(matches!(
        result.unwrap_err(),
        AgentError::UnexpectedToolCall
    ));
    assert!(events.is_empty(), "违约前不得发出事件: {events:?}");

    // 不推进：无助手步骤，会话回到待处理
    let steps = storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 1, "违约不得追加助手步骤: {steps:?}");
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Pending);
}

#[tokio::test]
async fn 助手消息先落库_完成事件后才释放() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit(&storage, dir.path(), "顺序").await;

    let llm = ScriptedLlm::new(vec![ScriptedTurn::Reply(vec!["回".into(), "复".into()])]);

    // 事件 sink 在事件到达时即刻回读存储，钉住顺序：
    // 增量事件期间助手消息尚未落库；完成事件发出时已落库；
    // 释放（状态回静止）在落库之后由既有断言覆盖
    let mut persisted_at_delta = true;
    let mut persisted_at_completed = false;
    {
        let mut emit = |event: Event| match event {
            Event::TokenDelta { .. } => {
                let steps = futures::executor::block_on(storage.steps(session_id)).unwrap();
                persisted_at_delta = steps.len() == 2;
            }
            Event::MessageCompleted { .. } => {
                let steps = futures::executor::block_on(storage.steps(session_id)).unwrap();
                persisted_at_completed = matches!(
                    steps.last(),
                    Some(Step::Message(Message::Assistant { text, .. })) if text == "回复"
                );
            }
            other => panic!("单轮剧本不得产出其他事件: {other:?}"),
        };
        Worker::new(&storage, &llm)
            .run_next_pending(&mut emit)
            .await
            .unwrap();
    }
    assert!(
        !persisted_at_delta,
        "增量事件先于落库发出：助手步骤不得提前落库"
    );
    assert!(
        persisted_at_completed,
        "完成事件发出前助手消息必须已落库（checkpoint 先行）"
    );
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 空token剧本_助手消息为空文本_会话仍静止() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit(&storage, dir.path(), "空回复").await;

    let llm = ScriptedLlm::new(vec![ScriptedTurn::Reply(vec![])]);
    let (ran, events) = run_collect(&storage, &llm).await;
    assert_eq!(ran.unwrap(), Some(session_id));

    // 无增量事件，直接完成；空文本也构成完整助手消息
    match &events[..] {
        [Event::MessageCompleted { message, .. }] => {
            assert_eq!(message.role(), Role::Assistant);
            assert_eq!(message.text(), "");
        }
        other => panic!("空剧本只应产出完成事件: {other:?}"),
    }
    let steps = storage.steps(session_id).await.unwrap();
    match &steps[..] {
        [Step::Message(user), Step::Message(assistant)] => {
            assert_eq!(user.role(), Role::User);
            assert_eq!(assistant.text(), "");
        }
        other => panic!("落库步骤不符: {other:?}"),
    }
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
}

#[tokio::test]
async fn 上下文路径为目录_读取失败向上传播且挂回待处理() {
    let dir = tempfile::tempdir().unwrap();
    // 与文件同名的目录：读取失败（非 NotFound）必须传播，不得静默跳过
    std::fs::create_dir(dir.path().join("AGENTS.md")).unwrap();
    let storage = open_storage(dir.path());
    let session_id = submit(&storage, dir.path(), "任务").await;

    let llm = ScriptedLlm::new(vec![ScriptedTurn::Reply(vec!["不应到达".into()])]);
    let (result, events) = run_collect(&storage, &llm).await;
    assert!(matches!(result.unwrap_err(), AgentError::PromptIo(_)));
    assert!(events.is_empty(), "提示组装失败不得发出事件: {events:?}");
    assert!(llm.captured().is_empty(), "提示组装失败不得请求 LLM");

    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Pending);
}

#[tokio::test]
async fn 队列为空_无事可做也不触碰替身() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let llm = ScriptedLlm::new(vec![]);

    let (result, events) = run_collect(&storage, &llm).await;
    assert_eq!(result.unwrap(), None);
    assert!(events.is_empty());
    assert!(llm.captured().is_empty());
}
