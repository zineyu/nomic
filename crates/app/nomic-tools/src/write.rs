//! `write` 工具：自动创建父目录 + 文件变更队列串行化（契约与 pi 一致）。

use std::path::Path;

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::mutation_queue::lock_path;

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteParams {
    /// Path to the file to write (relative or absolute)
    pub path: String,
    /// Content to write to the file
    pub content: String,
}

/// `write` 工具。
#[derive(Debug, Default, Clone, Copy)]
pub struct WriteTool;

const LABEL: &str = "write";

const DESCRIPTION: &str = "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. \
         Automatically creates parent directories.";

#[async_trait]
impl AgentTool for WriteTool {
    type Params = WriteParams;

    fn name(&self) -> &'static str {
        "write"
    }

    fn label(&self) -> &str {
        LABEL
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    async fn execute(
        &self,
        params: Self::Params,
        cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        let path = Path::new(&params.path);
        let _guard = lock_path(path).await;

        if cancel.is_cancelled() {
            return Err(ToolError::new("Operation aborted"));
        }
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ToolError::new(format!(
                    "Could not create parent directories for {}: {e}",
                    params.path
                ))
            })?;
        }
        tokio::fs::write(path, &params.content)
            .await
            .map_err(|e| ToolError::new(format!("Could not write file: {}. {e}", params.path)))?;
        if cancel.is_cancelled() {
            return Err(ToolError::new("Operation aborted"));
        }
        tracing::debug!(path = %params.path, bytes = params.content.len(), "file written");
        Ok(ToolResult::text(format!(
            "Successfully wrote {} bytes to {}",
            params.content.len(),
            params.path
        )))
    }
}
