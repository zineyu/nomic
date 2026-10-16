//! 工具协议领域类型。
//!
//! 每个工具声明名称、JSON Schema 参数与 replay-safe 标记；
//! 工具调用与结果均携带结构化负载，拒绝原因对客户端可见。

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

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
    /// 调用参数，语义由工具的 JSON Schema 定义。
    pub arguments: Value,
}

/// 工具执行结果：成功携带输出，拒绝携带原因。
///
/// 执行环境自身的故障（无法拉起进程等）经 [`crate::EnvironmentError`]
/// 传播；本类型只表达工具语义层面的成败。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResult {
    /// 执行成功。
    Ok {
        /// 工具输出（文本）。
        output: String,
    },
    /// 执行被拒绝（如越出工作目录边界）。
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

    /// 拒绝的便捷构造。
    #[must_use]
    pub fn rejected(reason: impl Into<String>) -> Self {
        Self::Rejected {
            reason: reason.into(),
        }
    }
}

/// 工具声明：名称、说明、JSON Schema 参数、replay-safe 标记。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// 工具名称，与 [`ToolCall::tool`] 对应。
    pub name: ToolName,
    /// 面向模型的工具说明。
    pub description: String,
    /// 参数 JSON Schema（`serde_json::Value`），由模型侧协议层校验。
    pub parameters: Value,
    /// 是否可安全重放：崩溃恢复时 replay-safe 工具自动续跑，
    /// 非 replay-safe 工具标记 interrupted 且不重复执行副作用。
    pub replay_safe: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ToolCallId;

    #[test]
    fn 空工具名_fast_fail() {
        let result = std::panic::catch_unwind(|| ToolName::new(""));
        assert!(result.is_err());
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
    fn tool_result_两分支可区分且往返一致() {
        let ok = ToolResult::ok("file contents");
        let rejected = ToolResult::rejected("path escapes workspace");
        let ok_json = serde_json::to_string(&ok).unwrap();
        let rejected_json = serde_json::to_string(&rejected).unwrap();
        assert_eq!(serde_json::from_str::<ToolResult>(&ok_json).unwrap(), ok);
        assert_eq!(
            serde_json::from_str::<ToolResult>(&rejected_json).unwrap(),
            rejected
        );
        assert!(matches!(
            serde_json::from_str::<ToolResult>(&ok_json).unwrap(),
            ToolResult::Ok { .. }
        ));
    }

    #[test]
    fn tool_spec_携带_replay_safe_标记() {
        let spec = ToolSpec {
            name: ToolName::new("shell"),
            description: "在工作目录内执行 shell 命令".into(),
            parameters: serde_json::json!({"type": "object"}),
            replay_safe: false,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let decoded: ToolSpec = serde_json::from_str(&json).unwrap();
        assert!(!decoded.replay_safe);
        assert_eq!(decoded, spec);
    }
}
