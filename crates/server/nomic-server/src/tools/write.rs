//! write 工具：写入文本文件（非 replay-safe，重复执行有副作用）。

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use nomic_core::{Tool, ToolError, ToolName};

use crate::tools::resolve_path;

/// write 输入。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteInput {
    /// 目标路径；相对路径基于会话工作目录解析，父目录不存在时自动创建。
    pub path: String,
    /// 写入的文本内容（覆盖既有内容）。
    pub content: String,
}

/// write 输出。
#[derive(Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct WriteOutput {
    /// 实际写入的路径。
    pub path: String,
    /// 写入的字节数。
    pub bytes: usize,
}

/// write：覆盖写入文本文件，父目录自动创建。
#[derive(Debug, Clone, Copy, Default)]
pub struct Write;

impl Tool for Write {
    type Input = WriteInput;
    type Output = WriteOutput;

    fn name(&self) -> ToolName {
        ToolName::new("write")
    }

    fn description(&self) -> &'static str {
        "覆盖写入文本文件；相对路径基于会话工作目录解析，父目录不存在时自动创建。"
    }

    fn replay_safe(&self) -> bool {
        false
    }

    async fn call(
        &self,
        working_dir: &Path,
        input: Self::Input,
    ) -> Result<Self::Output, ToolError> {
        let path = resolve_path(working_dir, &input.path);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                ToolError::Failed(format!("创建父目录 {} 失败: {error}", parent.display()))
            })?;
        }
        tokio::fs::write(&path, &input.content)
            .await
            .map_err(|error| ToolError::Failed(format!("写入 {} 失败: {error}", path.display())))?;
        Ok(WriteOutput {
            path: path.display().to_string(),
            bytes: input.content.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn 写入并自动创建父目录() {
        let dir = tempfile::tempdir().unwrap();
        let output = Write
            .call(
                dir.path(),
                WriteInput {
                    path: "src/nested/a.rs".into(),
                    content: "fn main() {}".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(output.bytes, 12);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/nested/a.rs")).unwrap(),
            "fn main() {}"
        );
    }

    #[tokio::test]
    async fn 覆盖既有内容() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "旧内容").unwrap();
        Write
            .call(
                dir.path(),
                WriteInput {
                    path: "a.txt".into(),
                    content: "新内容".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "新内容"
        );
    }
}
