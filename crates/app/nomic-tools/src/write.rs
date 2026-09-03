//! `write` 工具：自动创建父目录 + 文件变更队列串行化（契约与 pi 一致）。
//!
//! 内部 URI 目标先过 [`guard_writable`] 闸（ADR-0040 §8.1）：只读协议
//! 拒绝，可写协议经 router 分发到对应 VFS 实现。

use std::sync::Arc;

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use nomic_vfs::VfsRouter;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::mutation_queue::lock_path;
use crate::vfs_guard::guard_writable;

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
    /// VFS 挂载表；`None` 时 URI 目标一律落到文件系统分支
    vfs_router: Option<Arc<VfsRouter>>,
    /// 相对路径的解析基准（project 严格归属；空句柄 = 进程 cwd）
    base: crate::base::BaseDir,
}

impl WriteTool {
    /// 创建以进程 cwd 为基准的 write 工具。
    pub fn new() -> Self {
        Self::default()
    }

    /// 挂 VFS 挂载表（会话共享实例）。
    #[must_use]
    pub fn with_vfs_router(mut self, vfs_router: Arc<VfsRouter>) -> Self {
        self.vfs_router = Some(vfs_router);
        self
    }

    /// 设置固定基准目录：相对路径以它解析（project 严格归属）。
    #[must_use]
    pub fn with_base_dir(mut self, base_dir: Option<std::path::PathBuf>) -> Self {
        self.base = crate::base::BaseDir::new(base_dir);
        self
    }

    /// 共享基准目录句柄：句柄更新后本工具的下一次执行即用新基准
    ///（交互端切换 session 的 project 场景）。
    #[must_use]
    pub fn with_shared_base_dir(mut self, base: &crate::base::BaseDir) -> Self {
        self.base = base.clone();
        self
    }
}

const LABEL: &str = "write";

const DESCRIPTION: &str = "Write content to a file or a writable internal URI. Creates the file if it doesn't exist, overwrites if it does. \
         Automatically creates parent directories. Read-only internal URIs (e.g. skill://) are rejected; \
         use the protocol's dedicated tool to mutate those.";

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
        if let Some(router) = &self.vfs_router
            && let Some(target) = guard_writable(router, params.path.trim()).await?
        {
            target
                .router
                .write(&target.href, &params.content)
                .await
                .map_err(|error| ToolError::new(error.to_string()))?;
            tracing::debug!(uri = %target.href, bytes = params.content.len(), "uri written");
            return Ok(ToolResult::text(format!(
                "Successfully wrote {} bytes to {}",
                params.content.len(),
                params.path
            )));
        }
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
