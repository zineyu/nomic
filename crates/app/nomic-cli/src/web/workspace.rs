//! web 模式的 workspace 与 session 生命周期管理：[`Runtime`] 上的相关方法。
//!
//! workspace 是文件系统路径的一等实体（见 nomic-session 迁移 0005）：session
//! 创建时绑定 workspace，其所有操作以 workspace 路径为基准。这里的方法负责
//! 「按用户指定目录创建 session」「显式登记 workspace」与「删除 / 重命名」，
//! 并对用户输入的目录做存在性校验——不存在的目录返回 `BadRequest`，不静默
//! 登记无效路径。删除同时摘除注册表中对应的运行时（取消在途运行并关停，
//! 见 [`SessionRuntime::shutdown`](super::SessionRuntime::shutdown)）。

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

    /// 删除 session：摘除注册表中的运行时（在途运行取消、转发任务关停）
    /// 后物理删除；库中不存在时返回 `NotFound`。
    pub(crate) async fn delete_session(&self, session_id: &str) -> Result<(), ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        // 先摘除并关停运行时，避免删除落库后落库器继续追加（外键拒绝只告警）
        let session = self.sessions.lock().await.remove(session_id);
        if let Some(session) = session {
            session.shutdown();
        }
        if !store.delete_session(session_id).await? {
            return Err(ApiError::NotFound(format!(
                "session {session_id} not found"
            )));
        }
        Ok(())
    }

    /// 重命名 session：返回生效的自定义标题（`None` = 已清除，回退派生
    /// 标题）；session 不存在时返回 `NotFound`。
    pub(crate) async fn rename_session(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<Option<String>, ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        match store.rename_session(session_id, title).await {
            Ok(title) => Ok(title),
            Err(nomic_session::SessionError::SessionNotFound(_)) => Err(ApiError::NotFound(
                format!("session {session_id} not found"),
            )),
            Err(error) => Err(error.into()),
        }
    }

    /// 删除 workspace：默认拒绝非空（`BadRequest`，前端据此弹级联确认后
    /// 以 `force` 重试）；删除后摘除注册表中属于该 workspace 的全部
    /// session 运行时（含未落库口径外的空壳），返回被摘除的 session id
    /// 列表（向正在查看这些 session 的客户端广播用）。
    pub(crate) async fn delete_workspace(
        &self,
        id: &str,
        force: bool,
    ) -> Result<Vec<String>, ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        let Some(workspace) = store.workspace(id).await? else {
            return Err(ApiError::NotFound(format!("workspace {id} not found")));
        };
        match store.delete_workspace(id, force).await {
            Ok(_) => {}
            Err(error @ nomic_session::SessionError::WorkspaceNotEmpty { .. }) => {
                return Err(ApiError::BadRequest(format!("{error}")));
            }
            Err(error) => return Err(error.into()),
        }
        // 按路径匹配摘除该 workspace 名下的运行时（registry 不持 workspace
        // id，SessionRuntime.workspace 与 Workspace.path 同为规范化路径）
        let mut sessions = self.sessions.lock().await;
        let stale: Vec<String> = sessions
            .iter()
            .filter(|(_, session)| session.workspace == workspace.path)
            .map(|(id, _)| id.clone())
            .collect();
        let mut removed = Vec::with_capacity(stale.len());
        for id in stale {
            if let Some(session) = sessions.remove(&id) {
                session.shutdown();
                removed.push(id);
            }
        }
        Ok(removed)
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

    #[tokio::test]
    async fn delete_session_removes_runtime_and_store_row() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let created = state
            .inner
            .create_session(dir.path())
            .await
            .expect("create session");

        state
            .inner
            .delete_session(&created.id)
            .await
            .expect("delete session");
        assert!(
            !state.inner.sessions.lock().await.contains_key(&created.id),
            "注册表应摘除已删除的 session",
        );
        let store = state.inner.store.as_ref().expect("store");
        assert!(
            matches!(
                store.load_messages(&created.id).await,
                Err(nomic_session::SessionError::SessionNotFound(_))
            ),
            "库中 session 应物理删除",
        );
        // 未知 session 返回 NotFound
        assert!(matches!(
            state.inner.delete_session("no-such-session").await,
            Err(ApiError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn rename_session_roundtrip_and_not_found() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let created = state
            .inner
            .create_session(dir.path())
            .await
            .expect("create session");

        let title = state
            .inner
            .rename_session(&created.id, " 改名 ")
            .await
            .expect("rename");
        assert_eq!(title.as_deref(), Some("改名"));
        // 空白标题 = 清除自定义
        let title = state
            .inner
            .rename_session(&created.id, "  ")
            .await
            .expect("clear");
        assert_eq!(title, None);
        assert!(matches!(
            state.inner.rename_session("no-such-session", "x").await,
            Err(ApiError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn delete_workspace_refuses_non_empty_and_force_unregisters() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let created = state
            .inner
            .create_session(dir.path())
            .await
            .expect("create session");
        let store = state.inner.store.as_ref().expect("store");
        // 让 session 有一条 user 消息（非空 workspace）
        store
            .append_message(
                &created.id,
                None,
                &nomic_ai::Message::User(nomic_ai::UserMessage {
                    content: nomic_ai::UserMessageContent::Text("hi".to_string()),
                    timestamp: 1_000,
                }),
            )
            .await
            .expect("append");
        let workspace = store
            .workspace_of_session(&created.id)
            .await
            .expect("workspace")
            .expect("workspace row");

        // 默认拒绝非空
        assert!(matches!(
            state.inner.delete_workspace(&workspace.id, false).await,
            Err(ApiError::BadRequest(_))
        ));
        assert!(state.inner.sessions.lock().await.contains_key(&created.id));

        // force 级联：库中 session 删除 + 注册表摘除
        state
            .inner
            .delete_workspace(&workspace.id, true)
            .await
            .expect("force delete");
        assert!(store.workspace(&workspace.id).await.expect("q").is_none());
        assert!(
            !state.inner.sessions.lock().await.contains_key(&created.id),
            "force 删除应摘除该 workspace 名下的运行时",
        );
        // 未知 workspace 返回 NotFound
        assert!(matches!(
            state
                .inner
                .delete_workspace("no-such-workspace", true)
                .await,
            Err(ApiError::NotFound(_))
        ));
    }
}
