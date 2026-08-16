//! web 模式的 HTTP 层（axum）：REST 接口 + SSE 事件流 + 静态前端伺服。
//!
//! 安全：缺省只绑定 `127.0.0.1`（`--host` 显式覆盖）；POST 请求校验
//! `Origin` 头——非空且 host 不在本机集合、也不等于请求 `Host` 时拒绝
//! （DNS rebinding / 跨站请求防护，本服务能执行 bash）。不开放 CORS，
//! 开发期前端经 Vite 代理 `/api` 同源访问（见 `docs/adr/0030-web-ui.md`）。

use std::convert::Infallible;
use std::path::Path;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use nomic_ai::{Message, Model, ThinkingLevel};
use nomic_session::SessionSummary;
use nomic_tools::{AskUserAnswer, AskUserQuestion};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

use crate::model::ModelChoice;
use crate::web::{AppState, ServerEvent, Snapshot};

/// 组装路由：REST + SSE + 静态前端（产物存在时挂 `ServeDir`，SPA 回退
/// `index.html`；未构建时 fallback 返回提示文本）。
pub fn router(state: AppState, web_dist: &Path) -> Router {
    let mut app = Router::new()
        .route("/api/state", get(handle_state))
        .route("/api/stream", get(handle_stream))
        .route(
            "/api/sessions",
            get(handle_list_sessions).post(handle_create_session),
        )
        .route("/api/sessions/resume", post(handle_resume_session))
        .route(
            "/api/models",
            get(handle_list_models).post(handle_switch_model),
        )
        .route("/api/prompt", post(handle_prompt))
        .route("/api/cancel", post(handle_cancel))
        .route("/api/question/{id}", post(handle_question_answer))
        .route_layer(from_fn(reject_foreign_origin));

    if crate::web::has_frontend(web_dist) {
        let serve_dir =
            ServeDir::new(web_dist).not_found_service(ServeFile::new(web_dist.join("index.html")));
        app = app.fallback_service(serve_dir);
    } else {
        app = app.fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                "前端产物未构建：在 web/ 下运行 `npm run build`（或用 --web-dist 指定目录）",
            )
        });
    }
    app.with_state(state)
}

/// API 错误：统一转 HTTP 状态码 + `{"error": ...}` JSON。
pub enum ApiError {
    /// 内部错误（actor 退出、快照收集失败等）
    Internal(String),
    /// session 存储层错误
    Session(nomic_session::SessionError),
    /// session 库不可用（启动时已降级为不持久化）
    StoreUnavailable,
    /// 资源不存在（提问已过期等）
    NotFound(String),
    /// 请求非法（空 prompt、模型不存在等）
    BadRequest(String),
}

impl From<nomic_core::ActorError> for ApiError {
    fn from(error: nomic_core::ActorError) -> Self {
        tracing::error!(%error, "agent actor 调用失败");
        Self::Internal("agent actor 已退出".to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(%error, "内部错误");
        Self::Internal(format!("{error:#}"))
    }
}

impl From<nomic_session::SessionError> for ApiError {
    fn from(error: nomic_session::SessionError) -> Self {
        Self::Session(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
            Self::Session(error) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")),
            Self::StoreUnavailable => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "session 库不可用".to_string(),
            ),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

/// `GET /api/state`：当前快照（消息/模型/运行状态/待回答问题）。
#[derive(Serialize)]
pub struct StateResponse {
    pub messages: Vec<Message>,
    pub model: Model,
    pub reasoning: Option<ThinkingLevel>,
    pub context_tokens: u64,
    pub running: bool,
    pub queued: usize,
    pub session: Option<SessionInfo>,
    pub pending_question: Option<QuestionView>,
    pub cwd: String,
}

/// 当前 session 信息。
#[derive(Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: Option<String>,
}

/// 待回答问题（断线重连后前端恢复弹层用）。
#[derive(Serialize)]
pub struct QuestionView {
    pub id: String,
    pub question: AskUserQuestion,
}

/// `GET /api/models`：候选列表 + 当前选择。
#[derive(Serialize)]
pub struct ModelsResponse {
    pub current: crate::model::ModelSelection,
    pub candidates: Vec<ModelChoice>,
}

/// `GET /api/models` 处理。
async fn handle_list_models(
    State(state): State<AppState>,
) -> Result<Json<ModelsResponse>, ApiError> {
    let current = state.handle.model().await?;
    let current_selection = crate::model::ModelSelection {
        provider: current.provider,
        model: current.id,
    };
    let candidates = state.inner.models.candidates(&current_selection);
    Ok(Json(ModelsResponse {
        current: current_selection,
        candidates,
    }))
}

/// `POST /api/models`：切换模型（跨 provider 时按启动同一口径构造新连接并
/// 分层 api_key，与 TUI `/models` 一致）；选择结果落库（config 表 append-only）。
#[derive(Deserialize)]
pub struct ModelSwitchRequest {
    /// `<provider>/<模型id>` 全形式（无 `/` 时按当前 provider 解析）
    pub spec: String,
    /// 思考级别：`minimal` / `low` / `medium` / `high` / `off`（可选；
    /// 仅目标模型支持推理时随请求生效）
    pub reasoning: Option<String>,
}

/// `POST /api/models` 处理。
async fn handle_switch_model(
    State(state): State<AppState>,
    Json(request): Json<ModelSwitchRequest>,
) -> Result<Json<ModelChoice>, ApiError> {
    let current = state.handle.model().await?;
    let selection = crate::model::ModelSelection::parse(&request.spec, Some(&current.provider))
        .map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;
    let model = state
        .inner
        .models
        .resolve(&selection.provider, &selection.model)
        .map_err(|error| ApiError::BadRequest(format!("{error:#}")))?;

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
        state
            .handle
            .set_provider(
                crate::model::build_provider(model.api, api_key.clone()),
                api_key,
            )
            .map_err(|_| ApiError::Internal("agent actor 已退出".to_string()))?;
    }
    state
        .handle
        .set_model(model.clone())
        .map_err(|_| ApiError::Internal("agent actor 已退出".to_string()))?;
    if let Some(level) = request.reasoning.as_deref() {
        let level = parse_thinking_level(level)?;
        state
            .handle
            .set_reasoning(level)
            .map_err(|_| ApiError::Internal("agent actor 已退出".to_string()))?;
    }

    // 选择落库（与 TUI 同一口径）；失败仅告警不阻断切换
    let recorder = state.inner.recorder.lock().await;
    if let Some(recorder) = recorder.as_ref()
        && let Err(error) = recorder
            .store()
            .set_config(
                crate::model::CONFIG_KEY_MODEL,
                &serde_json::Value::String(selection.spec()),
            )
            .await
    {
        tracing::warn!(%error, "模型选择落库失败");
    }
    drop(recorder);

    Ok(Json(ModelChoice {
        provider: model.provider,
        id: model.id,
        name: model.name,
        context_window: model.context_window,
        reasoning: model.reasoning,
    }))
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

/// `GET /api/state` 处理。
async fn handle_state(State(state): State<AppState>) -> Result<Json<StateResponse>, ApiError> {
    let snapshot = crate::web::snapshot(&state).await?;
    Ok(Json(state_response(snapshot)))
}

fn state_response(snapshot: Snapshot) -> StateResponse {
    StateResponse {
        session: snapshot
            .session
            .map(|(id, title)| SessionInfo { id, title }),
        pending_question: snapshot
            .pending_question
            .map(|(id, question)| QuestionView { id, question }),
        messages: snapshot.messages,
        model: snapshot.model,
        reasoning: snapshot.reasoning,
        context_tokens: snapshot.context_tokens,
        running: snapshot.running,
        queued: snapshot.queued,
        cwd: snapshot.cwd.display().to_string(),
    }
}

/// `GET /api/stream`：SSE 事件流。客户端先取快照再订阅；事件负载为
/// [`ServerEvent`] JSON。订阅者落后（broadcast 淘汰旧事件）时收到
/// `refresh` 事件，前端重新拉取快照补齐。
async fn handle_stream(State(state): State<AppState>) -> Response {
    let rx = state.inner.events.subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(event) => Some((Ok::<_, Infallible>(server_event_to_sse(&event)), rx)),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(%skipped, "SSE 客户端落后，发送刷新提示");
                Some((
                    Ok::<_, Infallible>(
                        Event::default().event("refresh").data(skipped.to_string()),
                    ),
                    rx,
                ))
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    });
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// [`ServerEvent`] → SSE `data:` 负载（JSON）。
fn server_event_to_sse(event: &ServerEvent) -> Event {
    let payload = serde_json::to_string(event).unwrap_or_else(|error| {
        serde_json::json!({ "type": "error", "message": format!("序列化事件失败: {error}") })
            .to_string()
    });
    Event::default().data(payload)
}

/// `GET /api/sessions`：列出全部 session 摘要。
async fn handle_list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    let sessions = {
        let guard = state.inner.recorder.lock().await;
        let Some(recorder) = guard.as_ref() else {
            return Err(ApiError::StoreUnavailable);
        };
        let sessions = recorder.store().list_sessions().await?;
        drop(guard);
        sessions
    };
    Ok(Json(sessions))
}

/// `POST /api/sessions`：新建 session（新对话语义，清空 agent 上下文）。
async fn handle_create_session(
    State(state): State<AppState>,
) -> Result<Json<SessionInfo>, ApiError> {
    let cwd = std::env::current_dir()
        .map_err(|error| ApiError::BadRequest(format!("获取 cwd 失败: {error}")))?;
    let id = {
        let mut guard = state.inner.recorder.lock().await;
        let Some(recorder) = guard.as_mut() else {
            return Err(ApiError::StoreUnavailable);
        };
        let id = recorder.store().create_session(&cwd).await?;
        recorder.switch(id.clone(), None);
        drop(guard);
        id
    };
    // 清空 agent 上下文（fire-and-forget；随后的快照查询经邮箱 FIFO 必见）
    state
        .handle
        .clear_messages()
        .map_err(|_| ApiError::Internal("agent actor 已退出".to_string()))?;
    Ok(Json(SessionInfo { id, title: None }))
}

/// `POST /api/sessions/resume`：恢复指定 session（替换上下文并切换落库目标）。
#[derive(Deserialize)]
pub struct ResumeRequest {
    pub id: String,
}

/// `POST /api/sessions/resume` 处理。
async fn handle_resume_session(
    State(state): State<AppState>,
    Json(request): Json<ResumeRequest>,
) -> Result<Json<SessionInfo>, ApiError> {
    let (history, title) = {
        let mut guard = state.inner.recorder.lock().await;
        let Some(recorder) = guard.as_mut() else {
            return Err(ApiError::StoreUnavailable);
        };
        let history = recorder.store().load_messages(&request.id).await?;
        let title = nomic_session::session_title(&history);
        let tip = recorder.store().latest_entry_id(&request.id).await?;
        recorder.switch(request.id.clone(), tip);
        drop(guard);
        (history, title)
    };
    state
        .handle
        .restore_messages(history)
        .map_err(|_| ApiError::Internal("agent actor 已退出".to_string()))?;
    Ok(Json(SessionInfo {
        id: request.id,
        title,
    }))
}

/// `POST /api/prompt`：提交 prompt（空闲即跑，运行中入队）。
#[derive(Deserialize)]
pub struct PromptRequest {
    pub text: String,
    /// 图片附件（base64 内联；前端暂不提供上传，接口预留）
    #[serde(default)]
    pub images: Vec<nomic_ai::ImageContent>,
}

/// `POST /api/prompt` 处理。
async fn handle_prompt(
    State(state): State<AppState>,
    Json(request): Json<PromptRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if request.text.trim().is_empty() {
        return Err(ApiError::BadRequest("prompt 为空".to_string()));
    }
    let was_running = state.inner.gate.running();
    let started = crate::web::submit_prompt(&state, request.text, request.images).await;
    Ok(Json(serde_json::json!({
        "status": if was_running || !started { "queued" } else { "started" }
    })))
}

/// `POST /api/cancel`：取消当前轮运行。
async fn handle_cancel(State(state): State<AppState>) -> Json<serde_json::Value> {
    let cancelled = crate::web::cancel_run(&state).await;
    Json(serde_json::json!({ "cancelled": cancelled }))
}

/// `POST /api/question/{id}`：提交提问回答。
#[derive(Deserialize)]
pub struct QuestionAnswerRequest {
    pub answers: Vec<String>,
    #[serde(default)]
    pub custom: Option<String>,
}

/// `POST /api/question/{id}` 处理。
async fn handle_question_answer(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<QuestionAnswerRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let answer = AskUserAnswer {
        answers: request.answers,
        custom: request.custom,
    };
    if crate::web::answer_question(&state, &id, answer).await {
        Ok(Json(serde_json::json!({ "ok": true })))
    } else {
        Err(ApiError::NotFound(format!(
            "question {id} 不存在或已被回答"
        )))
    }
}

/// 状态变更请求的跨源防护：`Origin` 非空且 host 不在本机集合、也不等于
/// 请求 `Host` 时拒绝（本服务能执行 bash，CSRF 风险不可接受；GET 安全放行）。
async fn reject_foreign_origin(request: axum::http::Request<Body>, next: Next) -> Response {
    if request.method() == Method::POST {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !origin.is_empty() && !origin_allowed(origin, host) {
            tracing::warn!(%origin, "拒绝跨源 POST（CSRF 防护）");
            return (
                StatusCode::FORBIDDEN,
                "cross-origin request rejected".to_string(),
            )
                .into_response();
        }
    }
    next.run(request).await
}

/// Origin 是否可信：host 为本机回环地址，或与请求 Host 一致（LAN 场景）。
fn origin_allowed(origin: &str, host: &str) -> bool {
    let origin_host = origin
        .split("://")
        .nth(1)
        .unwrap_or(origin)
        .split('/')
        .next()
        .unwrap_or_default();
    let origin_host = strip_port(origin_host);
    let host_host = strip_port(host);
    matches!(origin_host, "127.0.0.1" | "localhost" | "::1" | "[::1]") || origin_host == host_host
}

/// 去掉 host 的端口后缀（IPv6 形式 `[::1]:3333` 的括号保留，匹配集合已含）。
fn strip_port(host: &str) -> &str {
    host.rsplit_once(':').map_or(host, |(host, _)| host)
}

#[cfg(test)]
mod tests {
    use super::origin_allowed;

    #[test]
    fn origin_allowed_accepts_loopback_and_same_host() {
        assert!(origin_allowed("http://localhost:5173", "127.0.0.1:3333"));
        assert!(origin_allowed("http://127.0.0.1:5173", "127.0.0.1:3333"));
        assert!(origin_allowed("http://[::1]:5173", "[::1]:3333"));
        // LAN：Origin host 与请求 Host 一致
        assert!(origin_allowed(
            "http://192.168.1.5:3333",
            "192.168.1.5:3333"
        ));
    }

    #[test]
    fn origin_allowed_rejects_foreign_origins() {
        assert!(!origin_allowed("http://evil.example.com", "127.0.0.1:3333"));
        assert!(!origin_allowed("https://attacker.io", "192.168.1.5:3333"));
        assert!(!origin_allowed(
            "http://localhost.evil.com",
            "127.0.0.1:3333"
        ));
    }
}
