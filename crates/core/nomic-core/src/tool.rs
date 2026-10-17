//! 工具协议领域类型。
//!
//! 每个工具实现 [`Tool`] trait：关联类型声明输入/输出，JSON Schema
//! 由类型自动生成；工具声明名称、说明与 replay-safe 标记。
//! 执行结果三层显式区分：成功输出、工具自身报错（回灌模型）、
//! 边界拒绝（原因对用户可见）；执行环境派发故障经
//! [`crate::EnvironmentError`] 单独传播。

use std::fmt;
use std::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::ToolCallId;

/// 工具名称，非空字符串。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolName(String);

impl ToolName {
    /// 构造工具名称；名称为空属于调用方错误，立即 panic 以 fast-fail。
    ///
    /// 工具名称由本仓库内置工具注册处提供，非外部输入。
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        assert!(!name.is_empty(), "tool name must not be empty");
        Self(name)
    }

    /// 名称文本。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// LLM 请求执行工具的一次调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// 调用 id，结果回灌与事件、落库步骤共用。
    pub id: ToolCallId,
    /// 目标工具。
    pub tool: ToolName,
    /// 调用参数，语义由工具的 input JSON Schema 定义。
    pub arguments: Value,
}

/// 工具执行结果：成功、工具自身报错、边界拒绝三层显式区分。
///
/// 三个 `Ok` 载荷分支都回灌模型；语义不同——`Ok` 是正常输出，
/// `Error` 是工具运行失败（消息面向模型与用户），`Rejected` 是
/// 显式拒绝：调用被接受前或执行中被策略拒绝，结构化原因对用户
/// 可见。执行环境无法派发调用（进程无法拉起等）经
/// [`crate::EnvironmentError`] 传播，不在本类型内。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResult {
    /// 执行成功。
    Ok {
        /// 工具输出（文本）。
        output: String,
    },
    /// 工具自身报错：已派发并运行，但执行失败。
    Error {
        /// 面向模型与用户的错误说明，原样回灌。
        message: String,
    },
    /// 显式拒绝：调用被接受前或执行中被策略拒绝。
    Rejected {
        /// 面向用户的拒绝原因。
        reason: String,
    },
}

impl ToolResult {
    /// 成功输出的便捷构造。
    #[must_use]
    pub fn ok(output: impl Into<String>) -> Self {
        Self::Ok {
            output: output.into(),
        }
    }

    /// 工具报错的便捷构造。
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// 拒绝的便捷构造。
    #[must_use]
    pub fn rejected(reason: impl Into<String>) -> Self {
        Self::Rejected {
            reason: reason.into(),
        }
    }
}

/// 工具自身执行失败的类别。
///
/// 与 [`crate::EnvironmentError`] 分工：本类型是工具语义层面的
/// 失败（回灌模型），`EnvironmentError` 是环境无法派发调用。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    /// 输入不满足工具前置条件（JSON Schema 初筛之后的语义校验）。
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// 工具执行失败；消息面向模型与用户，原样回灌。
    #[error("{0}")]
    Failed(String),
}

impl From<ToolError> for ToolResult {
    fn from(error: ToolError) -> Self {
        Self::error(error.to_string())
    }
}

/// 工具声明：名称、说明、input/output JSON Schema、replay-safe 标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// 工具名称，与 [`ToolCall::tool`] 对应。
    pub name: ToolName,
    /// 面向模型的工具说明。
    pub description: String,
    /// 输入参数 JSON Schema，由工具的 Input 关联类型自动生成。
    pub input_schema: Value,
    /// 输出 JSON Schema，由工具的 Output 关联类型自动生成。
    pub output_schema: Value,
    /// 是否可安全重放：崩溃恢复时 replay-safe 工具自动续跑，
    /// 非 replay-safe 工具标记 interrupted 且不重复执行副作用。
    pub replay_safe: bool,
}

/// 工具：声明 + 类型化调用。
///
/// 实现者为具体工具（读文件、写文件、shell、列目录、内容搜索），
/// 经执行环境持有与派发。input/output 的 JSON Schema 由
/// [`schemars`] 从关联类型自动生成，手写 schema 不产生第二事实源。
pub trait Tool: Send + Sync {
    /// 输入参数类型；JSON Schema 自动生成，协议层按此校验调用参数。
    type Input: serde::de::DeserializeOwned + schemars::JsonSchema;
    /// 输出类型；JSON Schema 自动生成，供文档与客户端展示。
    type Output: Serialize + schemars::JsonSchema;

    /// 工具名称。
    fn name(&self) -> ToolName;
    /// 面向模型的工具说明。
    fn description(&self) -> &'static str;
    /// 是否可安全重放。
    fn replay_safe(&self) -> bool;

    /// 生成工具声明：input/output JSON Schema 由关联类型自动生成。
    fn spec(&self) -> ToolSpec {
        fn schema<T: schemars::JsonSchema>() -> Value {
            // Schema 是纯数据结构，序列化不会失败
            serde_json::to_value(schemars::schema_for!(T))
                .expect("tool schema serialization is infallible")
        }
        ToolSpec {
            name: self.name(),
            description: self.description().to_owned(),
            input_schema: schema::<Self::Input>(),
            output_schema: schema::<Self::Output>(),
            replay_safe: self.replay_safe(),
        }
    }

    /// 执行工具。
    ///
    /// `working_dir` 为会话绑定的持久化工作区标识：相对路径经它解释
    /// 坐标（绝对路径原样使用），具体语义由工具定义。返回 `Ok` 为
    /// 成功输出；`Err(ToolError)` 为工具自身报错
    /// （经 [`ToolResult::Error`] 回灌模型）。
    fn call(
        &self,
        working_dir: &std::path::Path,
        input: Self::Input,
    ) -> impl Future<Output = Result<Self::Output, ToolError>> + Send;
}

#[cfg(test)]
mod trait_tests {
    //! Tool trait：关联类型 + 自动生成 JSON Schema + 三层结果区分。
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Deserialize, JsonSchema)]
    struct GreetInput {
        name: String,
    }

    #[derive(Debug, Serialize, JsonSchema, PartialEq)]
    struct GreetOutput {
        greeting: String,
    }

    struct GreetTool;

    impl Tool for GreetTool {
        type Input = GreetInput;
        type Output = GreetOutput;

        fn name(&self) -> ToolName {
            ToolName::new("greet")
        }

        fn description(&self) -> &'static str {
            "按名字生成问候语"
        }

        fn replay_safe(&self) -> bool {
            true
        }

        async fn call(
            &self,
            _working_dir: &std::path::Path,
            input: Self::Input,
        ) -> Result<Self::Output, ToolError> {
            Ok(GreetOutput {
                greeting: format!("你好，{}", input.name),
            })
        }
    }

    struct AlwaysFailTool;

    impl Tool for AlwaysFailTool {
        type Input = GreetInput;
        type Output = GreetOutput;

        fn name(&self) -> ToolName {
            ToolName::new("always_fail")
        }

        fn description(&self) -> &'static str {
            "必定失败的工具"
        }

        fn replay_safe(&self) -> bool {
            false
        }

        async fn call(
            &self,
            _working_dir: &std::path::Path,
            _input: Self::Input,
        ) -> Result<Self::Output, ToolError> {
            Err(ToolError::Failed("内部状态损坏".into()))
        }
    }

    #[test]
    fn spec_从关联类型自动生成_input_output_schema() {
        let spec = GreetTool.spec();
        assert_eq!(spec.name.as_str(), "greet");
        assert!(spec.replay_safe);
        let input = spec.input_schema.to_string();
        assert!(input.contains("name"), "input schema: {input}");
        let output = spec.output_schema.to_string();
        assert!(output.contains("greeting"), "output schema: {output}");
        // 声明可上线路由协议层传输
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<ToolSpec>(&json).unwrap(), spec);
    }

    #[test]
    fn call_执行成功返回输出() {
        let output = futures::executor::block_on(GreetTool.call(
            std::path::Path::new("/"),
            GreetInput {
                name: "nomic".into(),
            },
        ))
        .unwrap();
        assert_eq!(output.greeting, "你好，nomic");
    }

    #[test]
    fn 工具自身报错回灌为_error_分支且与成功_拒绝区分() {
        // 工具自身报错：Err(ToolError) → ToolResult::Error 回灌模型
        let tool_error = futures::executor::block_on(
            AlwaysFailTool.call(std::path::Path::new("/"), GreetInput { name: "x".into() }),
        )
        .unwrap_err();
        let result = ToolResult::from(tool_error);
        let json = serde_json::to_string(&result).unwrap();
        let decoded = serde_json::from_str::<ToolResult>(&json).unwrap();
        assert!(matches!(decoded, ToolResult::Error { ref message } if message == "内部状态损坏"));
        // 三层各自可区分
        assert!(matches!(ToolResult::ok("o"), ToolResult::Ok { .. }));
        assert!(matches!(
            ToolResult::rejected("被策略拒绝"),
            ToolResult::Rejected { .. }
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ToolCallId;

    #[test]
    #[should_panic(expected = "tool name must not be empty")]
    fn 空工具名_fast_fail() {
        let _ = ToolName::new("");
    }

    #[test]
    fn tool_call_json_往返保持值() {
        let call = ToolCall {
            id: ToolCallId::new(),
            tool: ToolName::new("read_file"),
            arguments: serde_json::json!({"path": "src/lib.rs"}),
        };
        let json = serde_json::to_string(&call).unwrap();
        assert_eq!(serde_json::from_str::<ToolCall>(&json).unwrap(), call);
    }

    #[test]
    fn tool_result_三层分支可区分且往返一致() {
        for result in [
            ToolResult::ok("file contents"),
            ToolResult::error("file not found"),
            ToolResult::rejected("path escapes workspace"),
        ] {
            let json = serde_json::to_string(&result).unwrap();
            assert_eq!(serde_json::from_str::<ToolResult>(&json).unwrap(), result);
        }
        assert!(matches!(
            serde_json::from_str::<ToolResult>(
                &serde_json::to_string(&ToolResult::error("e")).unwrap()
            )
            .unwrap(),
            ToolResult::Error { .. }
        ));
    }

    #[test]
    fn tool_error_转结果为_error_分支() {
        let invalid = ToolResult::from(ToolError::InvalidInput("缺少 path".into()));
        let failed = ToolResult::from(ToolError::Failed("磁盘错误".into()));
        assert!(
            matches!(invalid, ToolResult::Error { ref message } if message.contains("invalid input"))
        );
        assert!(matches!(failed, ToolResult::Error { .. }));
    }
}
