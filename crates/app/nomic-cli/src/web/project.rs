//! web 模式的 project 与 session 生命周期管理：[`Runtime`] 上的相关方法。
//!
//! project 是文件系统路径的一等实体（见 nomic-session 迁移 0005）：session
//! 创建时绑定 project，其所有操作以 project 路径为基准。这里的方法负责
//! 「按用户指定目录创建 session」「显式登记 project」与「删除 / 重命名」，
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
    /// 必须指定归属目录（无默认 project）：session 归属该目录对应的
    /// project（不存在则登记），工具基准取该目录的规范化路径。
    /// 指定的目录不存在或不是目录时返回 `BadRequest`，不会静默登记无效路径。
    pub(crate) async fn create_session(
        &self,
        project: &Path,
    ) -> Result<Arc<SessionRuntime>, ApiError> {
        let base = std::fs::canonicalize(project)
            .map_err(|_| ApiError::BadRequest(format!("目录不存在：{}", project.display())))?;
        if !base.is_dir() {
            return Err(ApiError::BadRequest(format!(
                "不是目录：{}",
                base.display()
            )));
        }
        // project 首次初始化：惰性生成默认 nix 环境定义（仅 nix 可用时；
        // ADR-0041）。失败不阻断，bash 会回退宿主环境
        if let Err(error) = nomic_tools::nix_env::ensure_default_flake(&base) {
            tracing::warn!(%error, "创建默认 nix 环境定义失败");
        }
        let id = match &self.store {
            Some(store) => store.create_session(&base).await?,
            None => uuid::Uuid::now_v7().to_string(),
        };
        let resolved = self
            .factory
            .resolve_session_model(self.store.as_ref(), &id)
            .await;
        // 新 session 归属于 base 对应的 project：工具基准即 base
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

    /// 列出全部 project 摘要（store 不可用时报错）。
    pub(crate) async fn list_projects(
        &self,
    ) -> Result<Vec<nomic_session::ProjectSummary>, ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        Ok(store.list_projects().await?)
    }

    /// 登记一个 project（按路径查或插，幂等），返回其 id 与规范化路径。
    ///
    /// 目录不存在或不是目录时返回 `BadRequest`：避免把用户输错的路径
    /// 静默登记成 project。
    pub(crate) async fn create_project(
        &self,
        path: &Path,
    ) -> Result<nomic_session::Project, ApiError> {
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
        // 登记即初始化：惰性生成默认 nix 环境定义（仅 nix 可用时；ADR-0041）
        if let Err(error) = nomic_tools::nix_env::ensure_default_flake(&canonical) {
            tracing::warn!(%error, "创建默认 nix 环境定义失败");
        }
        Ok(store.get_or_create_project(&canonical).await?)
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

    /// 删除 project：默认拒绝非空（`BadRequest`，前端据此弹级联确认后
    /// 以 `force` 重试）；删除后摘除注册表中属于该 project 的全部
    /// session 运行时（含未落库口径外的空壳），返回被摘除的 session id
    /// 列表（向正在查看这些 session 的客户端广播用）。
    pub(crate) async fn delete_project(
        &self,
        id: &str,
        force: bool,
    ) -> Result<Vec<String>, ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        let Some(project) = store.project(id).await? else {
            return Err(ApiError::NotFound(format!("project {id} not found")));
        };
        match store.delete_project(id, force).await {
            Ok(_) => {}
            Err(error @ nomic_session::SessionError::ProjectNotEmpty { .. }) => {
                return Err(ApiError::BadRequest(format!("{error}")));
            }
            Err(error) => return Err(error.into()),
        }
        // 按路径匹配摘除该 project 名下的运行时（registry 不持 project
        // id，SessionRuntime.project 与 Project.path 同为规范化路径）
        let mut sessions = self.sessions.lock().await;
        let stale: Vec<String> = sessions
            .iter()
            .filter(|(_, session)| session.project == project.path)
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
    async fn create_session_in_specified_project() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let created = state
            .inner
            .create_session(dir.path())
            .await
            .expect("create session in project");
        let canonical = std::fs::canonicalize(dir.path()).expect("canonical");
        assert_eq!(
            created.project, canonical,
            "session 操作基准应取 project 的规范化路径"
        );
        let store = state.inner.store.as_ref().expect("store");
        assert_eq!(
            store
                .session_project_path(&created.id)
                .await
                .expect("project path"),
            canonical,
        );
        // 同一路径再建 session：复用同一 project（get-or-create）
        let another = state
            .inner
            .create_session(dir.path())
            .await
            .expect("second session");
        let first = store.project_of_session(&created.id).await.expect("w1");
        let second = store.project_of_session(&another.id).await.expect("w2");
        assert_eq!(first.expect("project").id, second.expect("project").id,);
    }

    /// 系统提示词按 session 的 project 构建：project 祖先链上的
    /// AGENTS.md 注入提示词，cwd 脚注同为 project（严格归属）。
    #[tokio::test]
    async fn create_session_builds_system_prompt_from_project() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("AGENTS.md"), "project 专属规则").expect("write");
        let created = state
            .inner
            .create_session(dir.path())
            .await
            .expect("create session");
        // 提示词在 build 时构建完毕并随 agent 初始状态进入共享视图，
        // 查询无需 flush 屏障
        let prompt = created.handle.system_prompt().expect("查询应成功");
        assert!(
            prompt.contains("project 专属规则"),
            "project 的 AGENTS.md 应注入：{prompt}"
        );
        let canonical = std::fs::canonicalize(dir.path()).expect("canonical");
        assert!(
            prompt.contains(&format!(
                "Current working directory: {}",
                canonical.display()
            )),
            "cwd 脚注应为 session 的 project：{prompt}"
        );
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
    async fn create_project_is_idempotent_and_validates_dir() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let first = state
            .inner
            .create_project(dir.path())
            .await
            .expect("create project");
        let second = state
            .inner
            .create_project(dir.path())
            .await
            .expect("create again");
        assert_eq!(first.id, second.id, "同一路径应复用同一 project");
        let result = state
            .inner
            .create_project(Path::new("/nonexistent/nomic-test-dir"))
            .await;
        assert!(matches!(result, Err(ApiError::BadRequest(_))));
    }

    #[tokio::test]
    async fn list_projects_includes_registered() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        state
            .inner
            .create_project(dir.path())
            .await
            .expect("create project");
        let projects = state.inner.list_projects().await.expect("list");
        let canonical = std::fs::canonicalize(dir.path()).expect("canonical");
        assert!(
            projects.iter().any(|w| w.path == canonical),
            "列表应包含新登记的 project",
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
    async fn delete_project_refuses_non_empty_and_force_unregisters() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let created = state
            .inner
            .create_session(dir.path())
            .await
            .expect("create session");
        let store = state.inner.store.as_ref().expect("store");
        // 让 session 有一条 user 消息（非空 project）
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
        let project = store
            .project_of_session(&created.id)
            .await
            .expect("project")
            .expect("project row");

        // 默认拒绝非空
        assert!(matches!(
            state.inner.delete_project(&project.id, false).await,
            Err(ApiError::BadRequest(_))
        ));
        assert!(state.inner.sessions.lock().await.contains_key(&created.id));

        // force 级联：库中 session 删除 + 注册表摘除
        state
            .inner
            .delete_project(&project.id, true)
            .await
            .expect("force delete");
        assert!(store.project(&project.id).await.expect("q").is_none());
        assert!(
            !state.inner.sessions.lock().await.contains_key(&created.id),
            "force 删除应摘除该 project 名下的运行时",
        );
        // 未知 project 返回 NotFound
        assert!(matches!(
            state.inner.delete_project("no-such-project", true).await,
            Err(ApiError::NotFound(_))
        ));
    }
}
