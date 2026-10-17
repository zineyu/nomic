//! bash 工具：执行 shell 命令（非 replay-safe，重复执行有副作用）。
//!
//! 命令在工作目录内以 `sh -c` 执行；非零退出是正常输出的一部分
//! （exit_code 字段），不视为工具错误。

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use nomic_core::{Tool, ToolError, ToolName};

/// bash 输入。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashInput {
    /// 经 `sh -c` 执行的命令。
    pub command: String,
}

/// bash 输出。
#[derive(Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct BashOutput {
    /// 标准输出。
    pub stdout: String,
    /// 标准错误。
    pub stderr: String,
    /// 退出码；被信号终止时为 -1。
    pub exit_code: i32,
}

/// bash：在工作目录内执行命令。
#[derive(Debug, Clone, Copy, Default)]
pub struct Bash;

impl Tool for Bash {
    type Input = BashInput;
    type Output = BashOutput;

    fn name(&self) -> ToolName {
        ToolName::new("bash")
    }

    fn description(&self) -> &'static str {
        "在工作目录内执行 shell 命令（sh -c）；非零退出码在 exit_code 字段给出，stderr 原样返回。"
    }

    fn replay_safe(&self) -> bool {
        false
    }

    async fn call(
        &self,
        working_dir: &Path,
        input: Self::Input,
    ) -> Result<Self::Output, ToolError> {
        if input.command.trim().is_empty() {
            return Err(ToolError::InvalidInput("command 为空".into()));
        }
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&input.command)
            .current_dir(working_dir)
            .output()
            .await
            .map_err(|error| ToolError::Failed(format!("无法启动 shell: {error}")))?;
        Ok(BashOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn 命令在工作目录内执行() {
        let dir = tempfile::tempdir().unwrap();
        let output = Bash
            .call(
                dir.path(),
                BashInput {
                    command: "pwd && echo hello".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("hello"));
        assert!(output.stdout.trim_end().lines().next().is_some());
    }

    #[tokio::test]
    async fn 非零退出码作为正常输出() {
        let dir = tempfile::tempdir().unwrap();
        let output = Bash
            .call(
                dir.path(),
                BashInput {
                    command: "echo oops >&2; exit 3".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(output.exit_code, 3);
        assert!(output.stderr.contains("oops"));
    }

    #[tokio::test]
    async fn 空命令为_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let error = Bash
            .call(
                dir.path(),
                BashInput {
                    command: "   ".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidInput(_)), "实际: {error}");
    }
}
