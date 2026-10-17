//! S5（#40）独立复核：provider 错误正文回显密钥时的落点定性。
//!
//! 场景：provider 在错误响应正文里回显了它收到的 API key。探针记录
//! 该密钥最终出现在哪里：错误事件（广播面）出现，SQLite 落盘不出现。
//! 用于区分「标准 3 违规（落盘/日志）」与「加固缺口（广播面）」。

#[path = "common/agent_seam.rs"]
mod agent_seam;

use agent_seam::{ProtocolFixture, ScriptedEnv, next_session_event, subscribe_synced};
use nomic_ai::testing::{SCRIPTED_API_KEY, ScriptedRig, ScriptedTurn as RigTurn};
use nomic_core::{
    Acceptance, Event, EventErrorKind, LlmAdapter, Message, MessageId, RequestId, SessionId,
    SessionTarget, Submission,
};
use nomic_server::client::Client;

/// 提交一条用户消息到新会话，返回受理的会话 id。
async fn submit_new(client: &mut Client, dir: &std::path::Path, content: &str) -> SessionId {
    match client
        .submit(&Submission {
            request_id: RequestId::new(),
            target: SessionTarget::New,
            working_dir: dir.to_path_buf(),
            content: content.into(),
        })
        .await
        .unwrap()
    {
        Acceptance::Accepted { session_id } => session_id,
        Acceptance::Duplicate { .. } => panic!("首次提交不应判重"),
    }
}

#[tokio::test]
async fn provider_错误正文回显密钥_只进入错误事件不进入落盘() {
    let dir = tempfile::tempdir().unwrap();
    // 错误响应正文回显探针密钥，模拟恶意/异常 provider
    let echoed = format!("upstream echoed {SCRIPTED_API_KEY}");
    let llm = ScriptedRig::new(vec![RigTurn::TransportFailure(echoed.clone())]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "任务").await;

    subscribe_synced(&mut client, session_id).await;
    match next_session_event(&mut client).await {
        Event::Error { kind, message, .. } => {
            assert_eq!(kind, EventErrorKind::Llm);
            // 记录事实：错误事件确实承载 provider 正文
            assert!(
                message.contains(SCRIPTED_API_KEY),
                "错误事件未回显密钥（探针前提变化）: {message}"
            );
        }
        other => panic!("传输失败必须发出错误事件: {other:?}"),
    }

    // 落盘面：SQLite 文件不得含密钥
    let db = std::fs::read(dir.path().join("nomic.db")).unwrap();
    assert!(
        !String::from_utf8_lossy(&db).contains(SCRIPTED_API_KEY),
        "SQLite 落盘内容含 API key"
    );

    // 请求体不得含密钥（密钥只在 Authorization 头）
    for request in fixture.llm.sent() {
        assert!(
            !request.body.contains(SCRIPTED_API_KEY),
            "请求体含 API key: {}",
            request.body
        );
    }
}

#[tokio::test]
async fn 未配置_provider_错误事件给出修正指引() {
    // 显式失败而非静默兜底：提示含环境变量名
    let adapter = nomic_ai::RigAdapter::unconfigured();
    let error = adapter
        .respond(
            SessionId::new(),
            &[Message::User {
                id: MessageId::new(),
                text: "任务".into(),
            }],
            &[],
        )
        .await
        .err()
        .expect("未配置必须失败");
    let message = error.to_string();
    assert!(message.contains("NOMIC_API_KEY"), "实际: {message}");
    assert!(message.contains("NOMIC_MODEL"), "实际: {message}");
}
