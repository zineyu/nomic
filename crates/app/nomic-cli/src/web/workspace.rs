//! web 模式的 workspace 管理：[`Runtime`] 上的 workspace 相关方法。
//!
//! workspace 是文件系统路径的一等实体（见 nomic-session 迁移 0005）：session
//! 创建时绑定 workspace，其所有操作以 workspace 路径为基准。这里的方法负责
//! 「按用户指定目录创建 session」与「显式登记 workspace」，并对用户输入的
//! 目录做存在性校验——不存在的目录返回 `BadRequest`，不静默登记无效路径。

use std::path::Path;
use std::sync::Arc;

use super::api::ApiError;
use super::{Runtime, SessionRuntime};

impl Runtime {
    /// 新建一个 session：落库（可用时）+ 以进程默认模型构建 SessionRuntime。
    ///
    /// 必须指定归属目录（无默认 workspace）：session 归属该目录对应的
    /// workspace（不存在则登记），工具基准取该目录的规范化路径。
    /// 指定的目录不存在或不是目录时返回 `BadRequest`，不会静默登记无效路径。
    pub(crate) async fn create_session(
        &self,
        workspace: &Path,
    ) -> Result<Arc<SessionRuntime>, ApiError> {
        let base = std::fs::canonicalize(workspace)
            .map_err(|_| ApiError::BadRequest(format!("目录不存在：{}", workspace.display())))?;
        if !base.is_dir() {
            return Err(ApiError::BadRequest(format!(
                "不是目录：{}",
                base.display()
            )));
        }
        let id = match &self.store {
            Some(store) => store.create_session(&base).await?,
            None => uuid::Uuid::now_v7().to_string(),
        };
        let resolved = self
            .factory
            .resolve_session_model(self.store.as_ref(), &id)
            .await;
        // 新 session 归属于 base 对应的 workspace：工具基准即 base
        let session = self.factory.build(
            self.store.clone(),
            id.clone(),
            Vec::new(),
            None,
            base,
            resolved,
        );
        self.sessions
            .lock()
            .await
            .insert(id.clone(), session.clone());
        Ok(session)
    }

    /// 列出全部 workspace 摘要（store 不可用时报错）。
    pub(crate) async fn list_workspaces(
        &self,
    ) -> Result<Vec<nomic_session::WorkspaceSummary>, ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        Ok(store.list_workspaces().await?)
    }

    /// 登记一个 workspace（按路径查或插，幂等），返回其 id 与规范化路径。
    ///
    /// 目录不存在或不是目录时返回 `BadRequest`：避免把用户输错的路径
    /// 静默登记成 workspace。
    pub(crate) async fn create_workspace(
        &self,
        path: &Path,
    ) -> Result<nomic_session::Workspace, ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        let canonical = std::fs::canonicalize(path)
            .map_err(|_| ApiError::BadRequest(format!("目录不存在：{}", path.display())))?;
        if !canonical.is_dir() {
            return Err(ApiError::BadRequest(format!(
                "不是目录：{}",
                canonical.display()
            )));
        }
        Ok(store.get_or_create_workspace(&canonical).await?)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::web::api::ApiError;
    use crate::web::tests::test_state;

    #[tokio::test]
    async fn create_session_registers_independent_runtime() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let created = state
            .inner
            .create_session(dir.path())
            .await
            .expect("create session");
        assert_eq!(
            state.inner.sessions.lock().await.len(),
            2,
            "新 session 应注册进表"
        );
        assert!(state.inner.sessions.lock().await.contains_key(&created.id));
    }

    #[tokio::test]
    async fn create_session_in_specified_workspace() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let created = state
            .inner
            .create_session(dir.path())
            .await
            .expect("create session in workspace");
        let canonical = std::fs::canonicalize(dir.path()).expect("canonical");
        assert_eq!(
            created.workspace, canonical,
            "session 操作基准应取 workspace 的规范化路径"
        );
        let store = state.inner.store.as_ref().expect("store");
        assert_eq!(
            store
                .session_workspace_path(&created.id)
                .await
                .expect("workspace path"),
            canonical,
        );
        // 同一路径再建 session：复用同一 workspace（get-or-create）
        let another = state
            .inner
            .create_session(dir.path())
            .await
            .expect("second session");
        let first = store.workspace_of_session(&created.id).await.expect("w1");
        let second = store.workspace_of_session(&another.id).await.expect("w2");
        assert_eq!(first.expect("workspace").id, second.expect("workspace").id,);
    }

    #[tokio::test]
    async fn create_session_rejects_missing_dir() {
        let state = test_state().await;
        let result = state
            .inner
            .create_session(Path::new("/nonexistent/nomic-test-dir"))
            .await;
        assert!(
            matches!(result, Err(ApiError::BadRequest(_))),
            "不存在的目录应拒绝",
        );
    }

    #[tokio::test]
    async fn create_workspace_is_idempotent_and_validates_dir() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let first = state
            .inner
            .create_workspace(dir.path())
            .await
            .expect("create workspace");
        let second = state
            .inner
            .create_workspace(dir.path())
            .await
            .expect("create again");
        assert_eq!(first.id, second.id, "同一路径应复用同一 workspace");
        let result = state
            .inner
            .create_workspace(Path::new("/nonexistent/nomic-test-dir"))
            .await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[tokio::test]
    async fn list_workspaces_includes_registered() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        state
            .inner
            .create_workspace(dir.path())
            .await
            .expect("create workspace");
        let workspaces = state.inner.list_workspaces().await.expect("list");
        let canonical = std::fs::canonicalize(dir.path()).expect("canonical");
        assert!(
            workspaces.iter().any(|w| w.path == canonical),
            "列表应包含新登记的 workspace",
        );
    }
}
