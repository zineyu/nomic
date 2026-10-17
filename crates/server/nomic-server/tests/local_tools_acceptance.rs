//! 本地执行环境与内置工具验收（#38）：真实临时目录文件系统上的
//! 工具行为、replay-safe 声明可见性、结构化拒绝/报错结果。

use nomic_core::{
    EnvironmentError, ExecutionEnvironment, SessionId, ToolCall, ToolCallId, ToolName, ToolResult,
};
use nomic_server::environment::LocalEnvironment;
use nomic_server::tools::builtin_registry;

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

#[test]
fn 四工具注册表声明与_replay_safe_标记可见() {
    let specs = builtin_registry().specs();
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
    let env = LocalEnvironment::new();
    let session = SessionId::new();

    // read：相对路径经工作目录解释
    let output = env
        .execute(
            session,
            dir.path(),
            &call("read", serde_json::json!({"path": "README.md"})),
        )
        .await
        .unwrap();
    match output {
        ToolResult::Ok { output } => assert!(output.contains("# demo")),
        other => panic!("read 应为成功层: {other:?}"),
    }

    // write：创建文件后 read 读回
    let output = env
        .execute(
            session,
            dir.path(),
            &call(
                "write",
                serde_json::json!({"path": "src/new.rs", "content": "fn new() {}"}),
            ),
        )
        .await
        .unwrap();
    assert!(matches!(output, ToolResult::Ok { .. }));
    let content = std::fs::read_to_string(dir.path().join("src/new.rs")).unwrap();
    assert_eq!(content, "fn new() {}");

    // edit：唯一匹配块替换
    let output = env
        .execute(
            session,
            dir.path(),
            &call(
                "edit",
                serde_json::json!({
                    "path": "src/new.rs",
                    "old_text": "fn new() {}",
                    "new_text": "fn new() -> bool { true }"
                }),
            ),
        )
        .await
        .unwrap();
    assert!(matches!(output, ToolResult::Ok { .. }));
    let content = std::fs::read_to_string(dir.path().join("src/new.rs")).unwrap();
    assert_eq!(content, "fn new() -> bool { true }");

    // bash：在工作目录内执行
    let output = env
        .execute(
            session,
            dir.path(),
            &call("bash", serde_json::json!({"command": "cat README.md"})),
        )
        .await
        .unwrap();
    match output {
        ToolResult::Ok { output } => assert!(output.contains("# demo")),
        other => panic!("bash 应为成功层: {other:?}"),
    }
}

#[tokio::test]
async fn 工具自身报错为结构化_error_层() {
    let dir = setup_workspace();
    let env = LocalEnvironment::new();

    let output = env
        .execute(
            SessionId::new(),
            dir.path(),
            &call("read", serde_json::json!({"path": "nope.txt"})),
        )
        .await
        .unwrap();
    match output {
        ToolResult::Error { message } => {
            assert!(message.contains("nope.txt"), "实际: {message}");
        }
        other => panic!("缺失文件必须为工具错误层: {other:?}"),
    }

    // edit 匹配零处：结构化原因回灌模型
    let output = env
        .execute(
            SessionId::new(),
            dir.path(),
            &call(
                "edit",
                serde_json::json!({
                    "path": "README.md",
                    "old_text": "不存在的文本",
                    "new_text": "x"
                }),
            ),
        )
        .await
        .unwrap();
    match output {
        ToolResult::Error { message } => assert!(message.contains("无匹配"), "实际: {message}"),
        other => panic!("零处匹配必须为工具错误层: {other:?}"),
    }

    // 参数不满足输入类型：注册表反序列化归为 invalid input，同为 error 层
    let output = env
        .execute(
            SessionId::new(),
            dir.path(),
            &call("read", serde_json::json!({"path": 42})),
        )
        .await
        .unwrap();
    match output {
        ToolResult::Error { message } => assert!(message.contains("invalid input")),
        other => panic!("非法参数必须为工具错误层: {other:?}"),
    }
}

#[tokio::test]
async fn 未注册工具名归为_unknown_tool_环境故障() {
    let dir = setup_workspace();
    let env = LocalEnvironment::new();
    let error = env
        .execute(
            SessionId::new(),
            dir.path(),
            &call("delete_everything", serde_json::json!({})),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, EnvironmentError::UnknownTool(ref name) if name.as_str() == "delete_everything"),
        "实际: {error}"
    );
}

#[tokio::test]
async fn 绝对路径原样使用_不基于工作目录拼接() {
    let dir = setup_workspace();
    let env = LocalEnvironment::new();

    // 绝对路径指向工作目录之外的独立目录
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("outer.txt"), "外部内容").unwrap();

    let output = env
        .execute(
            SessionId::new(),
            dir.path(),
            &call(
                "read",
                serde_json::json!({"path": other.path().join("outer.txt")}),
            ),
        )
        .await
        .unwrap();
    match output {
        ToolResult::Ok { output } => assert!(output.contains("外部内容")),
        other_out => panic!("绝对路径 read 应为成功层: {other_out:?}"),
    }

    // 工作目录内不存在 outer.txt：证明未基于工作目录拼接解析
    assert!(!dir.path().join("outer.txt").exists());
}

#[tokio::test]
async fn bash_非零退出归为成功层_不视为工具错误() {
    let dir = tempfile::tempdir().unwrap();
    let env = LocalEnvironment::new();
    let output = env
        .execute(
            SessionId::new(),
            dir.path(),
            &call(
                "bash",
                serde_json::json!({"command": "echo oops >&2; exit 3"}),
            ),
        )
        .await
        .unwrap();
    match output {
        ToolResult::Ok { output } => {
            let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(parsed["exit_code"], serde_json::json!(3));
            assert!(parsed["stderr"].as_str().unwrap().contains("oops"));
        }
        other => panic!("非零退出必须为成功层（业务结果）: {other:?}"),
    }
}
