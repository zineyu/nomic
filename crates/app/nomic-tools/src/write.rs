//! `write` 工具：自动创建父目录 + 文件变更队列串行化（契约与 pi 一致）。

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
#[derive(Debug, Default, Clone)]
pub struct WriteTool {
    /// 相对路径的解析基准（workspace 严格归属；空句柄 = 进程 cwd）
    base: crate::base::BaseDir,
}

impl WriteTool {
    /// 创建以进程 cwd 为基准的 write 工具。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置固定基准目录：相对路径以它解析（workspace 严格归属）。
    #[must_use]
    pub fn with_base_dir(mut self, base_dir: Option<std::path::PathBuf>) -> Self {
        self.base = crate::base::BaseDir::new(base_dir);
        self
    }

    /// 共享基准目录句柄：句柄更新后本工具的下一次执行即用新基准
    ///（交互端切换 session 的 workspace 场景）。
    #[must_use]
    pub fn with_shared_base_dir(mut self, base: &crate::base::BaseDir) -> Self {
        self.base = base.clone();
        self
    }
}

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
        let base = self.base.snapshot();
        let path = crate::base::resolve(base.as_deref(), &params.path);
        let _guard = lock_path(&path).await;

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
        tokio::fs::write(&path, &params.content)
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
