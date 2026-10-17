//! 规格 §4.2 工具协议的验收测试（只经公共 API）。
//!
//! 覆盖：关联类型声明输入/输出、input/output JSON Schema 由类型自动
//! 生成、工具声明名称/说明/replay-safe 标记、结果三层显式区分、
//! 环境派发故障以错误单独传播。

use std::future::Future;

use nomic_core::{
    EnvironmentError, ExecutionEnvironment, SessionId, Tool, ToolCall, ToolCallId, ToolError,
    ToolName, ToolResult,
};

struct EchoTool;

impl Tool for EchoTool {
    type Input = String;
    type Output = String;

    fn name(&self) -> ToolName {
        ToolName::new("echo")
    }

    fn description(&self) -> &'static str {
        "原样返回输入文本"
    }

    fn replay_safe(&self) -> bool {
        true
    }

    async fn call(
        &self,
        _working_dir: &std::path::Path,
        input: Self::Input,
    ) -> Result<Self::Output, ToolError> {
        Ok(input)
    }
}

struct MutatingTool;

impl Tool for MutatingTool {
    type Input = bool;
    type Output = bool;

    fn name(&self) -> ToolName {
        ToolName::new("mutate")
    }

    fn description(&self) -> &'static str {
        "写入外部状态，重放不安全"
    }

    fn replay_safe(&self) -> bool {
        false
    }

    async fn call(
        &self,
        _working_dir: &std::path::Path,
        input: Self::Input,
    ) -> Result<Self::Output, ToolError> {
        Ok(input)
    }
}

struct BatchTool;

impl Tool for BatchTool {
    type Input = Vec<String>;
    type Output = bool;

    fn name(&self) -> ToolName {
        ToolName::new("batch")
    }

    fn description(&self) -> &'static str {
        "批量处理多条文本"
    }

    fn replay_safe(&self) -> bool {
        true
    }

    async fn call(
        &self,
        _working_dir: &std::path::Path,
        input: Self::Input,
    ) -> Result<Self::Output, ToolError> {
        Ok(input.len() % 2 == 0)
    }
}

#[test]
fn 声明覆盖名称_说明与_replay_safe_标记() {
    let echo_spec = EchoTool.spec();
    assert_eq!(echo_spec.name.as_str(), "echo");
    assert_eq!(echo_spec.description, "原样返回输入文本");
    assert!(echo_spec.replay_safe);

    let mutating_spec = MutatingTool.spec();
    assert_eq!(mutating_spec.name.as_str(), "mutate");
    assert_eq!(mutating_spec.description, "写入外部状态，重放不安全");
    assert!(!mutating_spec.replay_safe);
}

#[test]
fn input_output_schema_跟随关联类型自动生成() {
    // 基本类型：schema 的 type 与关联类型一致
    let echo_spec = EchoTool.spec();
    assert_eq!(echo_spec.input_schema["type"], serde_json::json!("string"));
    assert_eq!(echo_spec.output_schema["type"], serde_json::json!("string"));

    // input 与 output 各自独立生成：Input = Vec<String>，Output = bool
    let batch_spec = BatchTool.spec();
    assert_eq!(batch_spec.input_schema["type"], serde_json::json!("array"));
    assert_eq!(
        batch_spec.input_schema["items"]["type"],
        serde_json::json!("string")
    );
    assert_eq!(
        batch_spec.output_schema["type"],
        serde_json::json!("boolean")
    );
}

enum StubOutcome {
    Tool(ToolResult),
    SpawnFailure(String),
    IoFailure(String),
}

struct ScriptedEnv(StubOutcome);

impl ExecutionEnvironment for ScriptedEnv {
    fn execute(
        &self,
        _session: SessionId,
        _working_dir: &std::path::Path,
        _call: &ToolCall,
    ) -> impl Future<Output = Result<ToolResult, EnvironmentError>> + Send {
        let outcome = match &self.0 {
            StubOutcome::Tool(result) => Ok(result.clone()),
            StubOutcome::SpawnFailure(message) => Err(EnvironmentError::Spawn(message.clone())),
            StubOutcome::IoFailure(message) => Err(EnvironmentError::Io(message.clone())),
        };
        async move { outcome }
    }

    fn tool_specs(&self) -> &[nomic_core::ToolSpec] {
        &[]
    }
}

/// 调用方视角的分类：三层结果与环境故障必须可区分；环境错误为
/// `non_exhaustive`，未知类别归为 `env_other_failure` 而不是混入任一层。
const fn classify(outcome: &Result<ToolResult, EnvironmentError>) -> &'static str {
    match outcome {
        Ok(ToolResult::Ok { .. }) => "success",
        Ok(ToolResult::Error { .. }) => "tool_error",
        Ok(ToolResult::Rejected { .. }) => "rejected",
        Err(EnvironmentError::Spawn(_)) => "env_spawn_failure",
        Err(EnvironmentError::Io(_)) => "env_io_failure",
        Err(_) => "env_other_failure",
    }
}

#[test]
fn 环境派发故障独立于三层结果传播() {
    let session = SessionId::new();
    let call = ToolCall {
        id: ToolCallId::new(),
        tool: ToolName::new("echo"),
        arguments: serde_json::json!({}),
    };
    let cases = [
        (
            ScriptedEnv(StubOutcome::Tool(ToolResult::ok("输出"))),
            "success",
        ),
        (
            ScriptedEnv(StubOutcome::Tool(ToolResult::error("文件不存在"))),
            "tool_error",
        ),
        (
            ScriptedEnv(StubOutcome::Tool(ToolResult::rejected("越出工作目录"))),
            "rejected",
        ),
        (
            ScriptedEnv(StubOutcome::SpawnFailure("进程无法拉起".into())),
            "env_spawn_failure",
        ),
        (
            ScriptedEnv(StubOutcome::IoFailure("管道断开".into())),
            "env_io_failure",
        ),
    ];
    for (env, expected) in cases {
        let outcome =
            futures::executor::block_on(env.execute(session, std::path::Path::new("/"), &call));
        assert_eq!(classify(&outcome), expected);
    }
}
