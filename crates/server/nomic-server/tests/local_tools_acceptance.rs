//! 本地执行环境与内置工具验收（#38，S7 起协议驱动）：真实临时目录
//! 文件系统上的工具行为、replay-safe 声明可见性、结构化拒绝/报错
//! 结果。
//!
//! 工具执行路径经进程内 server 实例（真实 Unix socket）+ 协议客户端
//! 驱动：剧本化 LLM 请求工具调用，真实 LocalEnvironment 派发，结果
//! 经落库步骤与事件流断言（值断言与直调时期一致）。注册表声明测试
//! 不涉执行路径，保持直调。

#[path = "common/agent_seam.rs"]
mod agent_seam;

use agent_seam::{ProtocolFixture, ScriptedEvent, ScriptedLlm, ScriptedTurn, wait_for};
use nomic_core::{
    Acceptance, Event, RequestId, Role, SessionId, SessionState, SessionTarget, Step, Storage,
    Submission, ToolCall, ToolCallId, ToolName, ToolResult,
};
use nomic_server::environment::LocalEnvironment;

fn call(tool: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        tool: ToolName::new(tool),
        arguments,
    }
}

fn setup_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn answer() -> i32 { 42 }\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("README.md"), "# demo\n").unwrap();
    dir
}

/// 提交一条用户消息到新会话，返回受理的会话 id。
async fn submit_new(
    client: &mut nomic_server::client::Client,
    dir: &std::path::Path,
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

/// 剧本：每轮请求一次工具调用（闸门放行），最后一轮纯文本收尾。
fn tool_script(gate: &agent_seam::Gate, calls: Vec<ToolCall>) -> Vec<ScriptedTurn> {
    let mut turns: Vec<ScriptedTurn> = calls
        .into_iter()
        .map(|c| ScriptedTurn::PlayGated(gate.clone(), vec![ScriptedEvent::ToolCall(c)]))
        .collect();
    turns.push(ScriptedTurn::reply(["完成"]));
    turns
}

/// 会话内全部工具结果（按落库顺序）。
async fn finished_results(
    fixture: &ProtocolFixture<ScriptedLlm, LocalEnvironment>,
    session_id: SessionId,
) -> Vec<ToolResult> {
    fixture
        .storage
        .steps(session_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|step| match step {
            Step::ToolCallFinished { result, .. } => Some(result),
            _ => None,
        })
        .collect()
}

/// 等待会话静止（工具回合全部落库）。
async fn wait_idle(
    fixture: &ProtocolFixture<ScriptedLlm, LocalEnvironment>,
    session_id: SessionId,
) {
    wait_for(|| async {
        fixture
            .storage
            .get_session(session_id)
            .await
            .unwrap()
            .is_some_and(|session| session.state() == SessionState::Idle)
    })
    .await;
}

#[test]
fn 四工具注册表声明与_replay_safe_标记可见() {
    let specs = nomic_server::tools::builtin_registry().specs();
    let by_name = |name: &str| {
        specs
            .iter()
            .find(|spec| spec.name.as_str() == name)
            .unwrap_or_else(|| panic!("注册表缺少工具 {name}"))
    };
    for name in ["read", "write", "bash", "edit"] {
        let spec = by_name(name);
        assert!(!spec.description.is_empty(), "{name} 缺少说明");
        assert!(
            spec.input_schema.get("properties").is_some(),
            "{name} 缺少 input schema: {}",
            spec.input_schema
        );
        assert!(
            spec.output_schema.get("properties").is_some()
                || spec.output_schema.get("type").is_some(),
            "{name} 缺少 output schema: {}",
            spec.output_schema
        );
    }
    // replay-safe 标记：只读工具为真，写操作与 bash 为假
    assert!(by_name("read").replay_safe);
    assert!(!by_name("write").replay_safe);
    assert!(!by_name("bash").replay_safe);
    assert!(!by_name("edit").replay_safe);
}

#[tokio::test]
async fn 本地环境经注册表闭环执行四工具() {
    let dir = setup_workspace();
    let gate = agent_seam::gate();
    let calls = vec![
        call(
            "write",
            serde_json::json!({"path": "src/new.rs", "content": "fn new() {}"}),
        ),
        call(
            "edit",
            serde_json::json!({
                "path": "src/new.rs",
                "old_text": "fn new() {}",
                "new_text": "fn new() -> bool { true }"
            }),
        ),
        call("read", serde_json::json!({"path": "README.md"})),
        call("bash", serde_json::json!({"command": "cat README.md"})),
    ];
    let llm = ScriptedLlm::new(tool_script(&gate, calls));
    let fixture = ProtocolFixture::start(dir.path(), llm, LocalEnvironment::new()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "四工具").await;

    client.subscribe(session_id).await.unwrap();
    // 4 轮 × （调用请求 + 流终止） 各等待一次放行
    gate.release_all(8);
    wait_idle(&fixture, session_id).await;

    // write：创建文件；edit：唯一匹配块替换；read/bash：读出 README
    let results = finished_results(&fixture, session_id).await;
    assert_eq!(results.len(), 4, "四次调用各有一个结果: {results:?}");
    assert!(
        matches!(&results[0], ToolResult::Ok { .. }),
        "write: {results:?}"
    );
    assert!(
        matches!(&results[1], ToolResult::Ok { .. }),
        "edit: {results:?}"
    );
    assert!(
        matches!(&results[2], ToolResult::Ok { output } if output.contains("# demo")),
        "read: {results:?}"
    );
    assert!(
        matches!(&results[3], ToolResult::Ok { output } if output.contains("# demo")),
        "bash: {results:?}"
    );
    let content = std::fs::read_to_string(dir.path().join("src/new.rs")).unwrap();
    assert_eq!(content, "fn new() -> bool { true }");
}

#[tokio::test]
async fn 工具自身报错为结构化_error_层() {
    let dir = setup_workspace();
    let gate = agent_seam::gate();
    let calls = vec![
        call("read", serde_json::json!({"path": "nope.txt"})),
        call(
            "edit",
            serde_json::json!({
                "path": "README.md",
                "old_text": "不存在的文本",
                "new_text": "x"
            }),
        ),
        // 参数不满足输入类型：注册表反序列化归为 invalid input，同为 error 层
        call("read", serde_json::json!({"path": 42})),
    ];
    let llm = ScriptedLlm::new(tool_script(&gate, calls));
    let fixture = ProtocolFixture::start(dir.path(), llm, LocalEnvironment::new()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "三个报错").await;

    client.subscribe(session_id).await.unwrap();
    gate.release_all(6);
    wait_idle(&fixture, session_id).await;

    let results = finished_results(&fixture, session_id).await;
    assert_eq!(results.len(), 3, "三次调用各有一个结果: {results:?}");
    match &results[0] {
        ToolResult::Error { message } => {
            assert!(message.contains("nope.txt"), "实际: {message}");
        }
        other => panic!("缺失文件必须为工具错误层: {other:?}"),
    }
    match &results[1] {
        ToolResult::Error { message } => assert!(message.contains("无匹配"), "实际: {message}"),
        other => panic!("零处匹配必须为工具错误层: {other:?}"),
    }
    match &results[2] {
        ToolResult::Error { message } => assert!(message.contains("invalid input")),
        other => panic!("非法参数必须为工具错误层: {other:?}"),
    }
}

#[tokio::test]
async fn 未注册工具名归为_unknown_tool_环境故障() {
    let dir = setup_workspace();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::PlayGated(
            gate.clone(),
            vec![ScriptedEvent::ToolCall(call(
                "delete_everything",
                serde_json::json!({}),
            ))],
        ),
        ScriptedTurn::reply(["无法继续"]),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, LocalEnvironment::new()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "任务").await;

    agent_seam::subscribe_synced(&mut client, session_id).await;
    gate.release_all(2);

    // 派发故障经事件流到达：Environment 类别 + unknown tool 说明
    let events = agent_seam::read_session_events(&mut client, 3).await;
    assert!(events.iter().any(|event| matches!(
        event,
        Event::MessageCompleted { message, .. } if message.role() == Role::Assistant
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolCallStarted { .. }))
    );
    match events.last().unwrap() {
        Event::Error { kind, message, .. } => {
            assert_eq!(*kind, nomic_core::EventErrorKind::Environment);
            assert!(
                message.contains("unknown tool requested: delete_everything"),
                "实际: {message}"
            );
        }
        other => panic!("末事件必须是派发故障: {other:?}"),
    }
    // 调用以 interrupted 闭合（与 agent_loop 未知工具断言同一语义）
    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert!(matches!(
        steps.last(),
        Some(Step::ToolCallInterrupted { .. })
    ));
}

#[tokio::test]
async fn 绝对路径原样使用_不基于工作目录拼接() {
    let dir = setup_workspace();
    // 绝对路径指向工作目录之外的独立目录
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("outer.txt"), "外部内容").unwrap();

    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(tool_script(
        &gate,
        vec![call(
            "read",
            serde_json::json!({"path": other.path().join("outer.txt")}),
        )],
    ));
    let fixture = ProtocolFixture::start(dir.path(), llm, LocalEnvironment::new()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "读外部文件").await;

    client.subscribe(session_id).await.unwrap();
    gate.release_all(2);
    wait_idle(&fixture, session_id).await;

    let results = finished_results(&fixture, session_id).await;
    match &results[0] {
        ToolResult::Ok { output } => assert!(output.contains("外部内容")),
        other => panic!("绝对路径 read 应为成功层: {other:?}"),
    }
    // 工作目录内不存在 outer.txt：证明未基于工作目录拼接解析
    assert!(!dir.path().join("outer.txt").exists());
}

#[tokio::test]
async fn bash_非零退出归为成功层_不视为工具错误() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(tool_script(
        &gate,
        vec![call(
            "bash",
            serde_json::json!({"command": "echo oops >&2; exit 3"}),
        )],
    ));
    let fixture = ProtocolFixture::start(dir.path(), llm, LocalEnvironment::new()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "跑命令").await;

    client.subscribe(session_id).await.unwrap();
    gate.release_all(2);
    wait_idle(&fixture, session_id).await;

    let results = finished_results(&fixture, session_id).await;
    match &results[0] {
        ToolResult::Ok { output } => {
            let parsed: serde_json::Value = serde_json::from_str(output).unwrap();
            assert_eq!(parsed["exit_code"], serde_json::json!(3));
            assert!(parsed["stderr"].as_str().unwrap().contains("oops"));
        }
        other => panic!("非零退出必须为成功层（业务结果）: {other:?}"),
    }
}
