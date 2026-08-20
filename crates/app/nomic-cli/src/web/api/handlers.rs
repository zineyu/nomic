//! WebSocket 事件 handler：查询类（`get_state` / `list_models` / `list_sessions` /
//! `list_workspaces`）与命令类（`prompt` / `cancel` / `answer_question` /
//! `switch_model` / `create_session` / `create_workspace`）的具体实现，以及
//! 共享类型与辅助函数。

use std::sync::Arc;

use nomic_ai::{Message, Model, ThinkingLevel};
use nomic_tools::{AskUserAnswer, AskUserQuestion};
use serde::Serialize;

use super::ApiError;
use crate::model::ModelChoice;
use crate::web::{AppState, ServerEvent, Snapshot};

// ── 查询类 handler（返回带 request_id 的 ServerEvent）─────────────────────

/// 获取会话快照：消息历史、模型、思考级别、运行状态等。
pub async fn handle_get_state(state: &AppState, session_id: &str, request_id: &str) -> ServerEvent {
    let resolved = resolve_session_id(state, session_id);
    let result = async {
        let session = open_session(state, &resolved).await?;
        let snapshot = crate::web::snapshot(&session).await?;
        Ok::<_, ApiError>(snapshot)
    }
    .await;
    match result {
        Ok(snapshot) => ServerEvent::StateSnapshot {
            session_id: resolved,
            request_id: request_id.to_string(),
            snapshot: Box::new(SnapshotView::from_snapshot(snapshot)),
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 候选模型列表（跨 provider；当前选择由会话快照携带）。
pub fn handle_list_models(state: &AppState, request_id: &str) -> ServerEvent {
    let default_model = state.inner.factory.default_model.clone();
    let current = crate::model::ModelSelection {
        provider: default_model.provider,
        model: default_model.id,
    };
    let candidates = state.inner.models.candidates(&current);
    ServerEvent::ModelsList {
        request_id: request_id.to_string(),
        candidates,
    }
}

/// 列出全部 session 摘要。
pub async fn handle_list_sessions(state: &AppState, request_id: &str) -> ServerEvent {
    match state.inner.list_sessions().await {
        Ok(sessions) => ServerEvent::SessionsList {
            request_id: request_id.to_string(),
            sessions,
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 列出全部 workspace 摘要。
pub async fn handle_list_workspaces(state: &AppState, request_id: &str) -> ServerEvent {
    match state.inner.list_workspaces().await {
        Ok(workspaces) => ServerEvent::WorkspacesList {
            request_id: request_id.to_string(),
            workspaces,
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

// ── 命令类 handler（返回 ack ServerEvent）─────────────────────────────────

/// 提交 prompt（空闲即跑，运行中入队）；返回 ack 携带排队状态。
pub async fn handle_prompt(
    state: &AppState,
    session_id: &str,
    text: String,
    images: Vec<nomic_ai::ImageContent>,
) -> ServerEvent {
    if text.trim().is_empty() {
        return ApiError::BadRequest("prompt 为空".to_string()).to_ws_response(None);
    }
    let resolved = resolve_session_id(state, session_id);
    let session = match open_session(state, &resolved).await {
        Ok(s) => s,
        Err(error) => return error.to_ws_response(None),
    };
    let was_running = session.gate.running();
    let started = session.submit_prompt(text, images).await;
    ServerEvent::PromptAck {
        session_id: resolved,
        queued: was_running || !started,
    }
}

/// 取消当前轮运行。
pub async fn handle_cancel(state: &AppState, session_id: &str) -> ServerEvent {
    let resolved = resolve_session_id(state, session_id);
    let session = match open_session(state, &resolved).await {
        Ok(s) => s,
        Err(error) => return error.to_ws_response(None),
    };
    session.cancel_run().await;
    ServerEvent::CancelAck {
        session_id: resolved,
    }
}

/// 回答提问：回填 oneshot 通道。
pub async fn handle_answer_question(
    state: &AppState,
    session_id: &str,
    qid: String,
    answers: Vec<String>,
    custom: Option<String>,
) -> ServerEvent {
    let resolved = resolve_session_id(state, session_id);
    let session = match open_session(state, &resolved).await {
        Ok(s) => s,
        Err(error) => return error.to_ws_response(None),
    };
    let answer = AskUserAnswer { answers, custom };
    if session.answer_question(&qid, answer).await {
        ServerEvent::AnswerAck {
            session_id: resolved,
        }
    } else {
        ApiError::NotFound(format!("question {qid} 不存在或已被回答")).to_ws_response(None)
    }
}

/// 切换会话模型；结果落库到会话级 config。
pub async fn handle_switch_model(
    state: &AppState,
    session_id: &str,
    spec: String,
    reasoning: Option<String>,
) -> ServerEvent {
    let resolved = resolve_session_id(state, session_id);
    let session = match open_session(state, &resolved).await {
        Ok(s) => s,
        Err(error) => return error.to_ws_response(None),
    };
    let current = match session.handle.model().await {
        Ok(m) => m,
        Err(error) => {
            return ApiError::from(error).to_ws_response(None);
        }
    };
    let selection = match crate::model::ModelSelection::parse(&spec, Some(&current.provider)) {
        Ok(s) => s,
        Err(error) => {
            return ApiError::BadRequest(format!("{error:#}")).to_ws_response(None);
        }
    };
    let model = match state
        .inner
        .models
        .resolve(&selection.provider, &selection.model)
    {
        Ok(m) => m,
        Err(error) => {
            return ApiError::BadRequest(format!("{error:#}")).to_ws_response(None);
        }
    };

    if model.provider != current.provider {
        let api_key = crate::model::resolve_api_key(
            None,
            std::env::var(crate::model::api_key_env(model.api))
                .ok()
                .as_deref(),
            state
                .inner
                .models
                .provider_config(&model.provider)
                .and_then(|p| p.api_key.as_deref()),
            state
                .inner
                .models
                .config()
                .and_then(|c| c.api_key.as_deref()),
        );
        if session
            .handle
            .set_provider(
                crate::model::build_provider(model.api, api_key.clone()),
                api_key,
            )
            .is_err()
        {
            return ApiError::Internal("agent actor 已退出".to_string()).to_ws_response(None);
        }
    }
    if session.handle.set_model(model.clone()).is_err() {
        return ApiError::Internal("agent actor 已退出".to_string()).to_ws_response(None);
    }

    if let Some(level) = reasoning.as_deref() {
        match parse_thinking_level(level) {
            Ok(level) => {
                let _ = session.handle.set_reasoning(level);
                persist_session_reasoning(state, &resolved, level).await;
            }
            Err(error) => return error.to_ws_response(None),
        }
    }

    // 选择落库（会话级 config，与 TUI 同 append-only 口径）；失败仅告警
    persist_session_model(state, &resolved, &selection.spec()).await;

    ServerEvent::SwitchModelAck {
        session_id: resolved,
        choice: ModelChoice {
            provider: model.provider,
            id: model.id,
            name: model.name,
            context_window: model.context_window,
            reasoning: model.reasoning,
        },
    }
}

/// 新建 session（新对话语义，默认模型）；`workspace` 指定归属目录，
/// 缺省归属进程 cwd。
pub async fn handle_create_session(state: &AppState, workspace: Option<String>) -> ServerEvent {
    let workspace = match workspace.as_deref().map(expand_workspace_dir).transpose() {
        Ok(workspace) => workspace,
        Err(error) => return error.to_ws_response(None),
    };
    match state.inner.create_session(workspace.as_deref()).await {
        Ok(session) => ServerEvent::SessionCreated {
            id: session.id.clone(),
            title: None,
        },
        Err(error) => error.to_ws_response(None),
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

// ── 共享类型 ──────────────────────────────────────────────────────────────

/// 会话快照视图（WebSocket 响应携带，前端用于初始化/刷新状态）。
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotView {
    pub messages: Vec<Message>,
    pub model: Model,
    pub reasoning: Option<ThinkingLevel>,
    pub context_tokens: u64,
    pub running: bool,
    pub queued: usize,
    pub session: Option<(String, Option<String>)>,
    pub pending_question: Option<(String, AskUserQuestion)>,
    /// 本 session 的 workspace 路径（操作基准）
    pub workspace: String,
    /// 会话统计信息（前端状态栏展示用）
    #[serde(flatten)]
    pub stats: nomic_core::SessionStats,
}

impl SnapshotView {
    fn from_snapshot(snap: Snapshot) -> Self {
        Self {
            messages: snap.messages,
            model: snap.model,
            reasoning: snap.reasoning,
            context_tokens: snap.context_tokens,
            running: snap.running,
            queued: snap.queued,
            session: snap.session,
            pending_question: snap.pending_question,
            workspace: snap.workspace.display().to_string(),
            stats: snap.stats,
        }
    }
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────

/// 解析 session id 别名：`"default"` → 进程的 `default_session_id`。
fn resolve_session_id(state: &AppState, session_id: &str) -> String {
    if session_id == "default" {
        state.inner.default_session_id.clone()
    } else {
        session_id.to_string()
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

/// 从运行时打开（或惰性构建）指定 session（自动解析别名）。
async fn open_session(
    state: &AppState,
    id: &str,
) -> Result<Arc<crate::web::SessionRuntime>, ApiError> {
    state.inner.open_session(id).await
}

/// 会话级模型选择落库；库不可用或写失败仅告警不阻断切换。
async fn persist_session_model(state: &AppState, session_id: &str, spec: &str) {
    let Some(store) = &state.inner.store else {
        return;
    };
    if let Err(error) = store
        .set_session_config(
            session_id,
            crate::model::CONFIG_KEY_MODEL,
            &serde_json::Value::String(spec.to_string()),
        )
        .await
    {
        tracing::warn!(%error, "会话级模型选择落库失败");
    }
}

/// 会话级思考级别落库；库不可用或写失败仅告警不阻断切换。
async fn persist_session_reasoning(
    state: &AppState,
    session_id: &str,
    level: Option<ThinkingLevel>,
) {
    let Some(store) = &state.inner.store else {
        return;
    };
    let value = match level {
        Some(ThinkingLevel::Minimal) => "minimal",
        Some(ThinkingLevel::Low) => "low",
        Some(ThinkingLevel::Medium) => "medium",
        Some(ThinkingLevel::High) => "high",
        _ => "off",
    };
    if let Err(error) = store
        .set_session_config(
            session_id,
            crate::model::CONFIG_KEY_REASONING,
            &serde_json::Value::String(value.to_string()),
        )
        .await
    {
        tracing::warn!(%error, "会话级思考级别落库失败");
    }
}

/// 解析思考级别请求值；`off` → `None`（关闭）。
fn parse_thinking_level(level: &str) -> Result<Option<ThinkingLevel>, ApiError> {
    match level {
        "off" => Ok(None),
        "minimal" => Ok(Some(ThinkingLevel::Minimal)),
        "low" => Ok(Some(ThinkingLevel::Low)),
        "medium" => Ok(Some(ThinkingLevel::Medium)),
        "high" => Ok(Some(ThinkingLevel::High)),
        _ => Err(ApiError::BadRequest(format!(
            "--reasoning 取值非法：{level:?}（可选 minimal / low / medium / high / off）"
        ))),
    }
}
