//! session / workspace 生命周期命令 handler：创建（登记）、物理删除、
//! 重命名，以及共享的 ack 广播与目录展开辅助。

use super::super::ApiError;
use crate::web::{AppState, ServerEvent};

/// 新建 session（新对话语义，默认模型）；必须指定归属目录 `workspace`
/// （无默认 workspace；目录不存在或不是目录时拒绝，不静默登记无效路径）。
/// ack 携带 `request_id` 且经总线广播（其他客户端据此刷新列表）。
pub async fn handle_create_session(
    state: &AppState,
    request_id: &str,
    workspace: String,
) -> ServerEvent {
    let workspace = match expand_workspace_dir(&workspace) {
        Ok(workspace) => workspace,
        Err(error) => return error.to_ws_response(Some(request_id)),
    };
    match state.inner.create_session(&workspace).await {
        Ok(session) => broadcast_ack(
            state,
            ServerEvent::SessionCreated {
                request_id: request_id.to_string(),
                id: session.id.clone(),
                title: None,
            },
        ),
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 登记新 workspace（按路径查或插，幂等）；响应携带 `request_id` 供客户端关联。
pub async fn handle_create_workspace(
    state: &AppState,
    request_id: &str,
    path: String,
) -> ServerEvent {
    let path = match expand_workspace_dir(&path) {
        Ok(path) => path,
        Err(error) => return error.to_ws_response(Some(request_id)),
    };
    match state.inner.create_workspace(&path).await {
        Ok(workspace) => ServerEvent::WorkspaceCreated {
            request_id: request_id.to_string(),
            id: workspace.id,
            path: workspace.path.display().to_string(),
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 广播并返回一个 ack 事件：请求方经 `request_id` 关联响应，其他客户端
/// 经事件总线收到同一事件并刷新列表。
fn broadcast_ack(state: &AppState, event: ServerEvent) -> ServerEvent {
    let _ = state.inner.events.send(event.clone());
    event
}

/// 删除 session（物理删除；已打开的运行时一并摘除关停）。
pub async fn handle_delete_session(
    state: &AppState,
    request_id: &str,
    session_id: &str,
) -> ServerEvent {
    match state.inner.delete_session(session_id).await {
        Ok(()) => broadcast_ack(
            state,
            ServerEvent::SessionDeleted {
                request_id: Some(request_id.to_string()),
                id: session_id.to_string(),
            },
        ),
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 重命名 session；`title` 为生效的自定义标题（`None` = 已清除，回退派生）。
pub async fn handle_rename_session(
    state: &AppState,
    request_id: &str,
    session_id: &str,
    title: &str,
) -> ServerEvent {
    match state.inner.rename_session(session_id, title).await {
        Ok(title) => broadcast_ack(
            state,
            ServerEvent::SessionRenamed {
                request_id: request_id.to_string(),
                id: session_id.to_string(),
                title,
            },
        ),
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 删除 workspace：默认拒绝非空（error 事件提示剩余会话数，前端据此弹
/// 级联确认后以 `force` 重试）；删除后名下 session 运行时一并摘除关停，
/// 并向正在查看这些 session 的客户端广播 `session_deleted`（不带
/// request_id），使其跳出已失效的视图。
pub async fn handle_delete_workspace(
    state: &AppState,
    request_id: &str,
    id: &str,
    force: bool,
) -> ServerEvent {
    match state.inner.delete_workspace(id, force).await {
        Ok(removed) => {
            for session_id in removed {
                let _ = state.inner.events.send(ServerEvent::SessionDeleted {
                    request_id: None,
                    id: session_id,
                });
            }
            broadcast_ack(
                state,
                ServerEvent::WorkspaceDeleted {
                    request_id: request_id.to_string(),
                    id: id.to_string(),
                },
            )
        }
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 展开用户输入的 workspace 目录：去空白、`~/` 展开为家目录。
/// 空白输入返回 `BadRequest`；目录存在性由 `Runtime` 层校验。
fn expand_workspace_dir(input: &str) -> Result<std::path::PathBuf, ApiError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest("workspace 目录为空".to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        let home = dirs::home_dir()
            .ok_or_else(|| ApiError::Internal("无法定位家目录（~ 展开失败）".to_string()))?;
        return Ok(home.join(rest));
    }
    Ok(std::path::PathBuf::from(trimmed))
}

#[cfg(test)]
mod tests {
    use nomic_ai::Message;

    use super::*;
    use crate::web::ServerEvent;

    /// 新建 session：ack 携带 request_id 且经总线广播；目录不存在回 error。
    #[tokio::test]
    async fn create_session_ack_broadcasts_and_missing_dir_errors() {
        let state = crate::web::tests::test_state().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut events = state.inner.events.subscribe();

        let event =
            handle_create_session(&state, "r-new", dir.path().to_string_lossy().into_owned()).await;
        let ServerEvent::SessionCreated { request_id, id, .. } = event else {
            panic!("应返回 SessionCreated");
        };
        assert_eq!(request_id, "r-new");
        assert!(
            state.inner.sessions.lock().await.contains_key(&id),
            "新 session 应注册进表",
        );
        assert!(
            matches!(
                events.try_recv().expect("broadcast"),
                ServerEvent::SessionCreated { .. }
            ),
            "创建 ack 应广播到事件总线（其他客户端刷新列表）",
        );

        let event =
            handle_create_session(&state, "r-bad", "/nonexistent/nomic-test-dir".to_string()).await;
        let ServerEvent::Error { request_id, .. } = event else {
            panic!("不存在的目录应返回 error 事件");
        };
        assert_eq!(request_id.as_deref(), Some("r-bad"));
    }

    /// 删除 session：ack 携带 request_id 且经总线广播；未知 id 回 error。
    #[tokio::test]
    async fn delete_session_ack_broadcasts_and_unknown_errors() {
        let (state, session_id) = crate::web::tests::test_state_with_session().await;
        let mut events = state.inner.events.subscribe();

        let event = handle_delete_session(&state, "r-del", &session_id).await;
        let ServerEvent::SessionDeleted { request_id, id } = event else {
            panic!("应返回 SessionDeleted");
        };
        assert_eq!(request_id.as_deref(), Some("r-del"));
        assert_eq!(id, session_id);
        assert!(
            matches!(
                events.try_recv().expect("broadcast"),
                ServerEvent::SessionDeleted { .. }
            ),
            "删除 ack 应广播到事件总线",
        );
        assert!(
            !state.inner.sessions.lock().await.contains_key(&session_id),
            "注册表应摘除",
        );

        let event = handle_delete_session(&state, "r-del2", &session_id).await;
        assert!(
            matches!(event, ServerEvent::Error { .. }),
            "重复删除应返回 error 事件",
        );
    }

    /// 重命名 session：生效标题经 ack 返回；空白标题清除自定义（None）。
    #[tokio::test]
    async fn rename_session_ack_roundtrip() {
        let (state, session_id) = crate::web::tests::test_state_with_session().await;

        let event = handle_rename_session(&state, "r-ren", &session_id, "新名字").await;
        let ServerEvent::SessionRenamed {
            request_id, title, ..
        } = event
        else {
            panic!("应返回 SessionRenamed");
        };
        assert_eq!(request_id, "r-ren");
        assert_eq!(title.as_deref(), Some("新名字"));

        let event = handle_rename_session(&state, "r-ren2", &session_id, "  ").await;
        let ServerEvent::SessionRenamed { title, .. } = event else {
            panic!("应返回 SessionRenamed");
        };
        assert_eq!(title, None, "空白标题应清除自定义");

        let event = handle_rename_session(&state, "r-ren3", "no-such", "x").await;
        assert!(matches!(event, ServerEvent::Error { .. }));
    }

    /// 删除 workspace：非空默认拒绝（error 事件），force 级联删除并广播。
    #[tokio::test]
    async fn delete_workspace_refuse_then_force() {
        let (state, session_id) = crate::web::tests::test_state_with_session().await;
        let store = state.inner.store.as_ref().expect("store");
        store
            .append_message(
                &session_id,
                None,
                &Message::User(nomic_ai::UserMessage {
                    content: nomic_ai::UserMessageContent::Text("hi".to_string()),
                    timestamp: 1_000,
                }),
            )
            .await
            .expect("append");
        let workspace = store
            .workspace_of_session(&session_id)
            .await
            .expect("workspace")
            .expect("workspace row");

        let event = handle_delete_workspace(&state, "r-ws", &workspace.id, false).await;
        let ServerEvent::Error { message, .. } = event else {
            panic!("非空 workspace 应返回 error 事件");
        };
        assert!(message.contains("force"), "{message}");

        let mut events = state.inner.events.subscribe();
        let event = handle_delete_workspace(&state, "r-ws2", &workspace.id, true).await;
        // 级联删除名下已打开 session：先广播 session_deleted（不带 request_id）
        assert!(matches!(
            events.try_recv().expect("cascaded session_deleted"),
            ServerEvent::SessionDeleted {
                request_id: None,
                ..
            }
        ));
        let ServerEvent::WorkspaceDeleted { request_id, id } = event else {
            panic!("应返回 WorkspaceDeleted");
        };
        assert_eq!(request_id, "r-ws2");
        assert_eq!(id, workspace.id);
        assert!(matches!(
            events.try_recv().expect("broadcast"),
            ServerEvent::WorkspaceDeleted { .. }
        ));
        assert!(
            !state.inner.sessions.lock().await.contains_key(&session_id),
            "force 删除应摘除名下 session 运行时",
        );
    }
}
