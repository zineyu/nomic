//! web 模式的 HTTP 层（axum）：纯 WebSocket 事件驱动 + 静态前端伺服。
//!
//! 所有前端↔后端通信均通过 `ws://{host}/ws` 双向事件流：
//! - **客户端→服务端**：`ClientEvent`（JSON text frame，`type` 字段区分事件种类）
//! - **服务端→客户端**：`ServerEvent`（JSON text frame，`type` 字段区分事件种类）
//!
//! REST 接口已全部移除；查询类操作（state / models / sessions）通过带
//! `request_id` 的事件实现请求-响应关联，命令类操作（prompt / cancel）无显式
//! 响应，由服务端后续事件（`run_started` / `run_finished` / `error`）驱动前端状态。
//!
//! 安全：缺省只绑定 `127.0.0.1`（`--host` 显式覆盖）；WebSocket 连接校验
//! `Origin` 头——非空且 host 不在本机集合、也不等于请求 `Host` 时拒绝
//! （DNS rebinding / 跨站请求防护，本服务能执行 bash）。
//! 不开放 CORS，开发期前端经 Vite 代理 `/ws` 同源访问。

use axum::body::Body;
use axum::extract::ws::{self, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{Method, StatusCode, Uri, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use nomic_ai::{Message, Model, ThinkingLevel};
use nomic_tools::{AskUserAnswer, AskUserQuestion};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::model::ModelChoice;
use crate::web::{AppState, ServerEvent, Snapshot, assets};

// ── 客户端事件 ────────────────────────────────────────────────────────────

/// 客户端发送给服务端的事件（WebSocket text frame 负载；`type` 字段区分事件种类）。
///
/// 查询类事件（`get_state` / `list_models` / `list_sessions`）携带 `request_id`，
/// 服务端响应事件携带同一 `request_id` 供客户端关联；命令类事件（`prompt` / `cancel` /
/// `answer_question` / `switch_model` / `create_session`）为 fire-and-forget，
/// 由服务端后续 `ServerEvent` 驱动状态更新。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    /// 查询当前会话快照（消息/模型/运行状态/待回答问题）。
    GetState { request_id: String },
    /// 查询候选模型列表。
    ListModels { request_id: String },
    /// 列出全部 session 摘要。
    ListSessions { request_id: String },
    /// 提交 prompt（空闲即跑，运行中入队）。
    Prompt {
        text: String,
        #[serde(default)]
        images: Vec<nomic_ai::ImageContent>,
    },
    /// 取消当前轮运行。
    Cancel,
    /// 回答提问。
    AnswerQuestion {
        id: String,
        answers: Vec<String>,
        #[serde(default)]
        custom: Option<String>,
    },
    /// 切换会话模型。
    SwitchModel {
        spec: String,
        #[serde(default)]
        reasoning: Option<String>,
    },
    /// 新建 session。
    CreateSession,
}

// ── 组装路由 ──────────────────────────────────────────────────────────────

/// 组装路由：WebSocket 事件流 + 静态前端（内嵌 `web/dist`，见 [`assets`]；
/// 未命中路径 SPA 回退 `index.html`）。
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/ws", get(handle_ws))
        .route_layer(from_fn(reject_foreign_origin))
        .fallback(|uri: Uri| async move { assets::serve(uri.path()) })
        .with_state(state)
}

// ── API 错误 ──────────────────────────────────────────────────────────────

/// API 错误：内部用于 handler 链，统一转 WebSocket error 事件。
#[derive(Debug)]
pub enum ApiError {
    Internal(String),
    Session(nomic_session::SessionError),
    StoreUnavailable,
    NotFound(String),
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
        let (status, message) = match &self {
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
            Self::Session(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
            Self::StoreUnavailable => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "session 库不可用".to_string(),
            ),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m.clone()),
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
        };
        (status, message).into_response()
    }
}

impl ApiError {
    fn to_ws_response(&self, request_id: Option<&str>) -> ServerEvent {
        let message = match self {
            Self::Internal(m) => m.clone(),
            Self::Session(e) => format!("{e:#}"),
            Self::StoreUnavailable => "session 库不可用".to_string(),
            Self::NotFound(m) => m.clone(),
            Self::BadRequest(m) => m.clone(),
        };
        ServerEvent::Error {
            request_id: request_id.map(str::to_string),
            message,
        }
    }
}

// ── WebSocket 处理 ────────────────────────────────────────────────────────

/// `GET /ws`：会话级双向 WebSocket 事件流。连接时自动使用默认 session。
///
/// 客户端发送 `ClientEvent`（JSON text frame），服务端响应 `ServerEvent`
/// （JSON text frame）。查询事件通过 `request_id` 关联；命令事件由服务端后续
/// 生命周期事件（`run_started` / `run_finished` / `error` 等）驱动状态。
async fn handle_ws(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    // 使用默认 session（启动时已构建，保证至少一个 session 存在）
    let session = open_session(&state, &state.inner.default_session_id).await?;
    let rx = session.events.subscribe();
    let shutdown = state.inner.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| ws_session(socket, state, rx, shutdown)))
}

/// WebSocket 会话：双向通信——客户端发 `ClientEvent`，服务端推 `ServerEvent`。
async fn ws_session(
    mut socket: WebSocket,
    state: AppState,
    mut rx: tokio::sync::broadcast::Receiver<ServerEvent>,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = shutdown.clone().cancelled_owned() => {
                let _ = socket.send(ws::Message::Close(Some(ws::CloseFrame {
                    code: 1001,
                    reason: "server shutting down".into(),
                }))).await;
                break;
            }
            // 服务端→客户端：broadcast 事件推给客户端
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        let payload = serde_json::to_string(&event).unwrap_or_else(|error| {
                            serde_json::json!({ "type": "error", "message": format!("序列化事件失败: {error}") })
                                .to_string()
                        });
                        if socket.send(ws::Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(%skipped, "WebSocket 客户端落后，发送刷新提示");
                        let payload = serde_json::to_string(&ServerEvent::Refresh)
                            .unwrap_or_else(|_| "{\"type\":\"refresh\"}".to_string());
                        if socket.send(ws::Message::Text(payload.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // 客户端→服务端：处理 ClientEvent
            msg = socket.recv() => {
                match msg {
                    Some(Ok(ws::Message::Text(text))) => {
                        match serde_json::from_str::<ClientEvent>(&text) {
                            Ok(event) => {
                                let response = dispatch(&state, event).await;
                                let payload = serde_json::to_string(&response)
                                    .unwrap_or_else(|e| format!(r#"{{"type":"error","message":"序列化失败: {e}"}}"#));
                                if socket.send(ws::Message::Text(payload.into())).await.is_err() {
                                    break;
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "解析客户端事件失败");
                                let msg = serde_json::json!({
                                    "type": "error",
                                    "message": format!("事件解析失败: {error}")
                                }).to_string();
                                let _ = socket.send(ws::Message::Text(msg.into())).await;
                            }
                        }
                    }
                    Some(Ok(_)) => {}, // 忽略非文本帧
                    _ => break,        // 客户端断开或协议错误
                }
            }
        }
    }
}

/// 分发客户端事件到对应 handler，返回服务端响应事件。
async fn dispatch(state: &AppState, event: ClientEvent) -> ServerEvent {
    let result = match event {
        // ── 命令类（fire-and-forget，返回 ack 或由后续事件驱动）──
        ClientEvent::Prompt { text, images } => handle_prompt(state, text, images).await,
        ClientEvent::Cancel => handle_cancel(state).await,
        ClientEvent::AnswerQuestion {
            id,
            answers,
            custom,
        } => handle_answer_question(state, id, answers, custom).await,
        ClientEvent::SwitchModel { spec, reasoning } => {
            handle_switch_model(state, spec, reasoning).await
        }
        ClientEvent::CreateSession => handle_create_session(state).await,
        // ── 查询类（携带 request_id，响应也带同一 request_id）──
        ClientEvent::GetState { request_id } => {
            return handle_get_state(state, &request_id).await;
        }
        ClientEvent::ListModels { request_id } => {
            return handle_list_models(state, &request_id).await;
        }
        ClientEvent::ListSessions { request_id } => {
            return handle_list_sessions(state, &request_id).await;
        }
    };
    match result {
        Ok(event) => event,
        Err(error) => error.to_ws_response(None),
    }
}

// ── 查询类 handler（返回带 request_id 的 ServerEvent）─────────────────────

/// 获取会话快照：消息历史、模型、思考级别、运行状态等。
async fn handle_get_state(state: &AppState, request_id: &str) -> ServerEvent {
    let result = async {
        let sessions = state.inner.sessions.lock().await;
        let session = sessions
            .values()
            .next()
            .ok_or_else(|| ApiError::NotFound("无活跃会话".to_string()))?;
        let snapshot = crate::web::snapshot(session).await?;
        Ok::<_, ApiError>(snapshot)
    }
    .await;
    match result {
        Ok(snapshot) => ServerEvent::StateSnapshot {
            request_id: request_id.to_string(),
            snapshot: SnapshotView::from_snapshot(snapshot),
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 候选模型列表（跨 provider；当前选择由会话快照携带）。
async fn handle_list_models(state: &AppState, request_id: &str) -> ServerEvent {
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
async fn handle_list_sessions(state: &AppState, request_id: &str) -> ServerEvent {
    match state.inner.list_sessions().await {
        Ok(sessions) => ServerEvent::SessionsList {
            request_id: request_id.to_string(),
            sessions,
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

// ── 命令类 handler（返回 ack ServerEvent）─────────────────────────────────

/// 提交 prompt（空闲即跑，运行中入队）；返回 ack 携带排队状态。
async fn handle_prompt(state: &AppState, text: String, images: Vec<nomic_ai::ImageContent>) -> Result<ServerEvent, ApiError> {
    if text.trim().is_empty() {
        return Err(ApiError::BadRequest("prompt 为空".to_string()));
    }
    let sessions = state.inner.sessions.lock().await;
    let session = sessions
        .values()
        .next()
        .ok_or_else(|| ApiError::NotFound("无活跃会话".to_string()))?
        .clone();
    drop(sessions);
    let was_running = session.gate.running();
    let started = session.submit_prompt(text, images).await;
    Ok(ServerEvent::PromptAck {
        queued: was_running || !started,
    })
}

/// 取消当前轮运行。
async fn handle_cancel(state: &AppState) -> Result<ServerEvent, ApiError> {
    let sessions = state.inner.sessions.lock().await;
    let session = sessions
        .values()
        .next()
        .ok_or_else(|| ApiError::NotFound("无活跃会话".to_string()))?
        .clone();
    drop(sessions);
    session.cancel_run().await;
    Ok(ServerEvent::CancelAck)
}

/// 回答提问：回填 oneshot 通道。
async fn handle_answer_question(
    state: &AppState,
    qid: String,
    answers: Vec<String>,
    custom: Option<String>,
) -> Result<ServerEvent, ApiError> {
    let sessions = state.inner.sessions.lock().await;
    let session = sessions
        .values()
        .next()
        .ok_or_else(|| ApiError::NotFound("无活跃会话".to_string()))?
        .clone();
    drop(sessions);
    let answer = AskUserAnswer { answers, custom };
    if session.answer_question(&qid, answer).await {
        Ok(ServerEvent::AnswerAck)
    } else {
        Err(ApiError::NotFound(format!(
            "question {qid} 不存在或已被回答"
        )))
    }
}

/// 切换会话模型；结果落库到会话级 config。
async fn handle_switch_model(
    state: &AppState,
    spec: String,
    reasoning: Option<String>,
) -> Result<ServerEvent, ApiError> {
    let sessions = state.inner.sessions.lock().await;
    let session = sessions
        .values()
        .next()
        .ok_or_else(|| ApiError::NotFound("无活跃会话".to_string()))?
        .clone();
    drop(sessions);
    let current = session.handle.model().await?;
    let selection = crate::model::ModelSelection::parse(&spec, Some(&current.provider))
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
        session
            .handle
            .set_provider(
                crate::model::build_provider(model.api, api_key.clone()),
                api_key,
            )
            .map_err(|_| ApiError::Internal("agent actor 已退出".to_string()))?;
    }
    session
        .handle
        .set_model(model.clone())
        .map_err(|_| ApiError::Internal("agent actor 已退出".to_string()))?;

    if let Some(level) = reasoning.as_deref() {
        let level = parse_thinking_level(level)?;
        session
            .handle
            .set_reasoning(level)
            .map_err(|_| ApiError::Internal("agent actor 已退出".to_string()))?;
        persist_session_reasoning(state, &session.id, level).await;
    }

    // 选择落库（会话级 config，与 TUI 同 append-only 口径）；失败仅告警
    persist_session_model(state, &session.id, &selection.spec()).await;

    Ok(ServerEvent::SwitchModelAck {
        choice: ModelChoice {
            provider: model.provider,
            id: model.id,
            name: model.name,
            context_window: model.context_window,
            reasoning: model.reasoning,
        },
    })
}

/// 新建 session（新对话语义，默认模型）。
async fn handle_create_session(state: &AppState) -> Result<ServerEvent, ApiError> {
    let session = state.inner.create_session().await?;
    Ok(ServerEvent::SessionCreated {
        id: session.id.clone(),
        title: None,
    })
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
    pub cwd: String,
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
            cwd: snap.cwd.display().to_string(),
            stats: snap.stats,
        }
    }
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────

/// 从运行时打开（或惰性构建）指定 session。
async fn open_session(
    state: &AppState,
    id: &str,
) -> Result<std::sync::Arc<crate::web::SessionRuntime>, ApiError> {
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

// ── 跨源防护 ──────────────────────────────────────────────────────────────

/// WebSocket 连接的跨源防护：`Origin` 非空且 host 不在本机集合、也不等于
/// 请求 `Host` 时拒绝（本服务能执行 bash，CSRF 风险不可接受）。
async fn reject_foreign_origin(request: axum::http::Request<Body>, next: Next) -> Response {
    if request.method() == Method::GET {
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
            tracing::warn!(%origin, "拒绝跨源请求（CSRF 防护）");
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

    /// 停机令牌取消后 WebSocket 连接必须关闭：graceful shutdown 等所有在途
    /// 连接收尾，不关闭的话退出键按下后进程挂住（回归测试）。
    #[tokio::test]
    async fn ws_ends_when_shutdown_cancelled() {
        use axum::Router;
        use axum::routing::get;
        use futures::StreamExt;

        let state = crate::web::tests::test_state().await;

        let app = Router::new()
            .route("/ws", get(super::handle_ws))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(axum::serve(listener, app).into_future());

        let url = format!("ws://{addr}/ws");
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("ws connect");

        // 取消停机令牌——服务端应关闭 WebSocket 连接
        state.inner.shutdown.cancel();

        // 读取直到连接关闭或超时
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(msg) = ws_stream.next().await {
                if matches!(
                    msg,
                    Err(_) | Ok(tokio_tungstenite::tungstenite::Message::Close(_))
                ) {
                    return true;
                }
            }
            true // stream ended
        })
        .await;

        server.abort();
        assert!(result.is_ok(), "停机后 WebSocket 未在 5 秒内关闭");
    }

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
