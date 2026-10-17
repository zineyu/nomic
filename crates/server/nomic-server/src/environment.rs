//! 本地执行环境：在同机进程内派发内置工具注册表。
//!
//! 会话绑定的 `working_dir` 由派发方（worker）随调用传入，
//! 相对路径坐标系在工具内统一解释；会话标识保留为关联键，
//! 供后续远程派发与取消（S10）使用。

use std::future::Future;
use std::path::Path;

use nomic_core::{
    EnvironmentError, ExecutionEnvironment, SessionId, ToolCall, ToolRegistry, ToolResult, ToolSpec,
};

use crate::tools::builtin_registry;

/// 本地执行环境：工具调用经内置注册表派发到同机文件系统与进程。
#[derive(Debug)]
pub struct LocalEnvironment {
    registry: ToolRegistry,
    specs: Vec<ToolSpec>,
}

impl LocalEnvironment {
    /// 以内置工具集创建本地执行环境。
    pub fn new() -> Self {
        let registry = builtin_registry();
        let specs = registry.specs();
        Self { registry, specs }
    }
}

impl Default for LocalEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionEnvironment for LocalEnvironment {
    fn execute(
        &self,
        _session: SessionId,
        working_dir: &Path,
        call: &ToolCall,
    ) -> impl Future<Output = Result<ToolResult, EnvironmentError>> + Send {
        let outcome = self
            .registry
            .call(&call.tool, working_dir, call.arguments.clone())
            .map_err(|unknown| EnvironmentError::UnknownTool(unknown.0));
        async move {
            match outcome {
                Ok(invocation) => Ok(invocation.await),
                Err(error) => Err(error),
            }
        }
    }

    fn tool_specs(&self) -> &[ToolSpec] {
        &self.specs
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use nomic_core::ToolName;

    use super::*;

    #[tokio::test]
    async fn 未注册工具名归为_unknown_tool() {
        let env = LocalEnvironment::new();
        let call = ToolCall {
            id: nomic_core::ToolCallId::new(),
            tool: ToolName::new("nope"),
            arguments: serde_json::json!({}),
        };
        let error = env
            .execute(SessionId::new(), Path::new("/"), &call)
            .await
            .unwrap_err();
        assert!(
            matches!(error, EnvironmentError::UnknownTool(ref name) if name.as_str() == "nope"),
            "实际: {error}"
        );
    }

    #[tokio::test]
    async fn 内置注册表四工具齐全() {
        let env = LocalEnvironment::new();
        let names: Vec<&str> = env
            .tool_specs()
            .iter()
            .map(|spec| spec.name.as_str())
            .collect();
        assert_eq!(names, ["read", "write", "bash", "edit"]);
    }
}
