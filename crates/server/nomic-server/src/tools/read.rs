//! read 工具：读取文本文件内容（replay-safe）。

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use nomic_core::{Tool, ToolError, ToolName};

use crate::tools::resolve_path;

/// read 输入。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadInput {
    /// 文件路径；相对路径基于会话工作目录解析。
    pub path: String,
}

/// read 输出。
#[derive(Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct ReadOutput {
    /// 文件文本内容。
    pub content: String,
}

/// read：读取文本文件内容。文件不存在或不可读时报工具错误
/// （经 `ToolResult::Error` 回灌模型）。
#[derive(Debug, Clone, Copy, Default)]
pub struct Read;

impl Tool for Read {
    type Input = ReadInput;
    type Output = ReadOutput;

    fn name(&self) -> ToolName {
        ToolName::new("read")
    }

    fn description(&self) -> &'static str {
        "读取文本文件内容；相对路径基于会话工作目录解析。文件不存在或不可读时返回工具错误。"
    }

    fn replay_safe(&self) -> bool {
        true
    }

    async fn call(
        &self,
        working_dir: &Path,
        input: Self::Input,
    ) -> Result<Self::Output, ToolError> {
        let path = resolve_path(working_dir, &input.path);
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| ToolError::Failed(format!("读取 {} 失败: {error}", path.display())))?;
        Ok(ReadOutput { content })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn 读取存在的文件返回内容() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "你好 nomic").unwrap();
        let output = Read
            .call(
                dir.path(),
                ReadInput {
                    path: "a.txt".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(output.content, "你好 nomic");
    }

    #[tokio::test]
    async fn 文件不存在报工具错误() {
        let dir = tempfile::tempdir().unwrap();
        let error = Read
            .call(
                dir.path(),
                ReadInput {
                    path: "missing.txt".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Failed(_)), "实际: {error}");
    }
}
