//! serve 模式的 HTTP 层（axum）：纯 WebSocket 事件流（headless，无静态伺服）。
//!
//! GUI ↔后端通信均通过 `ws://{host}/ws` 双向事件流：
//! - **客户端→服务端**：`ClientEvent`（JSON text frame，`type` 字段区分事件种类）
//! - **服务端→客户端**：`ServerEvent`（JSON text frame，`type` 字段区分事件种类）
//!
//! 事件架构：进程级全局事件总线（[`crate::state::EventBus`]，注册在 [`crate::state::AppState`]
//! 中，ADR-0047），所有 session 的生命周期
//! 事件直接发往总线，每个事件携带 `session_id` 供前端路由。WebSocket 连接只需
//! 订阅总线一次，即可接收全部 session 的事件——无需订阅管理。
//!
//! 查询类事件（`get_state` / `list_models` / `list_sessions`）携带 `request_id`
//! 实现请求-响应关联；命令类事件（`prompt` / `cancel` 等）携带 `session_id`
//! 指定目标 session，fire-and-forget，由服务端后续事件驱动前端状态。
//!
//! 安全：缺省只绑定 `127.0.0.1`（`--host` 显式覆盖）；WebSocket 连接校验
//! `Origin` 头——非空且 host 不在本机集合、也不等于请求 `Host` 时拒绝
//! （DNS rebinding / 跨站请求防护，本服务能执行 bash）。不开放 CORS；
//! Flutter GUI 为非浏览器客户端，默认不发送 `Origin` 头，不受此限制。

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{self, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{Method, StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::Instrument as _;

use crate::serve::{ServerEvent, WebState};

mod handlers;

pub use handlers::{SettingsSnapshotView, SkillItem, SnapshotView};

// ── 客户端事件 ────────────────────────────────────────────────────────────

/// 客户端发送给服务端的事件（WebSocket text frame 负载；`type` 字段区分事件种类）。
///
/// - 查询类事件（`get_state` / `list_models` / `list_sessions`）携带 `request_id`，
///   服务端响应事件携带同一 `request_id` 供客户端关联。
/// - 命令类事件（`prompt` / `cancel` 等）为 fire-and-forget，携带 `session_id`
///   指定目标 session，由服务端后续 `ServerEvent` 驱动状态更新。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    /// 查询当前会话快照（消息/模型/运行状态/待回答问题）。
    GetState {
        session_id: String,
        request_id: String,
    },
    /// 查询候选模型列表。
    ListModels { request_id: String },
    /// 列出全部 work 摘要（侧栏列表的一等入口，ADR-0044）。
    ListWorks { request_id: String },
    /// 列出一个 work 下的 session（含子 agent session；侧栏展开与只读
    /// 回溯入口，ADR-0044）。
    ListWorkSessions { request_id: String, work_id: String },
    /// 列出全部 project 摘要。
    ListProjects { request_id: String },
    /// 查询 skill 清单（`@skill://` 补全用；进程级 skill 解析器快照）。
    ListSkills { request_id: String },
    /// 查询文件候选（`@file:` 补全用；相对目标 session 的 project 前缀匹配）。
    ListFiles {
        session_id: String,
        prefix: String,
        request_id: String,
    },
    /// 提交 prompt（空闲即跑；运行中普通文本入 steering 队列——turn 边界
    /// 注入本轮，斜杠命令仍走 runner 串行 job 队列等待本轮结束）。
    Prompt {
        session_id: String,
        text: String,
        #[serde(default)]
        images: Vec<nomic_ai::ImageContent>,
    },
    /// 编辑 steering 队列条目原文（空文本删除该条目，oil.nvim 空行忽略
    /// 语义；附件保留）。fire-and-forget，变更经 `queue_changed` 广播。
    UpdateQueueEntry {
        session_id: String,
        id: String,
        text: String,
    },
    /// 删除 steering 队列条目。fire-and-forget，变更经 `queue_changed` 广播。
    RemoveQueueEntry { session_id: String, id: String },
    /// 移动 steering 队列条目（上移/下移一位）。fire-and-forget，变更经
    /// `queue_changed` 广播。
    MoveQueueEntry {
        session_id: String,
        id: String,
        direction: MoveDirection,
    },
    /// 取消当前轮运行。
    Cancel { session_id: String },
    /// 回答提问。
    AnswerQuestion {
        session_id: String,
        id: String,
        answers: Vec<String>,
        #[serde(default)]
        custom: Option<String>,
    },
    /// 切换会话模型。
    SwitchModel {
        session_id: String,
        spec: String,
        #[serde(default)]
        reasoning: Option<String>,
    },
    /// 新建 work（查询式命令：携带 `request_id`，响应 `work_created`
    /// 或 error 事件带同一 `request_id`，ack 同时经总线广播供其他客户端
    /// 刷新列表）。新对话语义，默认模型；必须指定归属目录 `project`
    /// （无默认 project；不存在则报错，不会静默归属进程 cwd）。
    /// 连带创建主 session，`work_created.session_id` 即打开目标。
    CreateWork { request_id: String, project: String },
    /// 登记新 project（查询式命令：携带 `request_id`，响应 `project_created`
    /// 或 error 事件带同一 `request_id`；按路径查或插，幂等）。
    CreateProject { request_id: String, path: String },
    /// 删除 work（查询式命令：响应 `work_deleted` 或 error 事件带同一
    /// `request_id`；级联物理删除名下全部 session，entries 与会话级
    /// config 经外键级联清除）。
    DeleteWork { request_id: String, id: String },
    /// 重命名 work（查询式命令：响应 `work_renamed` 或 error 事件；
    /// `title` 裁剪后为空 = 清除自定义标题，回退派生标题）。
    RenameWork {
        request_id: String,
        id: String,
        title: String,
    },
    /// 删除 project（查询式命令：响应 `project_deleted` 或 error 事件；
    /// 默认拒绝非空 project，`force` 级联删除名下全部 session）。
    DeleteProject {
        request_id: String,
        id: String,
        #[serde(default)]
        force: bool,
    },
    /// 查询设置快照（providers + 模型覆盖 + 标量全量；ADR-0039）。
    GetSettings { request_id: String },
    /// 新建或更新 provider（查询式命令：逐字段补丁三态——字段缺失 =
    /// 不更新，null = 清除；响应 `settings_updated` 并广播 `settings_changed`）。
    UpsertProvider {
        request_id: String,
        name: String,
        #[serde(flatten)]
        patch: nomic_session::ProviderPatch,
    },
    /// 删除 provider（其模型覆盖级联清除）。
    DeleteProvider { request_id: String, name: String },
    /// 新建或更新模型覆盖（逐字段补丁三态同 `upsert_provider`；所属
    /// provider 必须已定义）。
    UpsertModelSpec {
        request_id: String,
        provider: String,
        model_id: String,
        #[serde(flatten)]
        patch: nomic_session::ModelSpecPatch,
    },
    /// 删除模型覆盖。
    DeleteModelSpec {
        request_id: String,
        provider: String,
        model_id: String,
    },
    /// 写入标量设置（键与取值类型校验与 `nomic config set` 同一口径）。
    SetSetting {
        request_id: String,
        key: String,
        value: serde_json::Value,
    },
    /// 删除标量设置。
    UnsetSetting { request_id: String, key: String },
}

// ── 组装路由 ──────────────────────────────────────────────────────────────

/// 队列条目移动方向（`move_queue_entry` 命令）。
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveDirection {
    /// 上移一位（队首方向）
    Up,
    /// 下移一位（队尾方向）
    Down,
}

/// 组装路由：仅 WebSocket 事件流（`/ws`）。serve 模式不伺服任何静态
/// 资源——GUI 为独立的 Flutter 应用（ADR-0046）。
pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/ws", get(handle_ws))
        .route_layer(from_fn(reject_foreign_origin))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
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
        tracing::error!(?error, "agent actor call failed");
        Self::Internal("agent actor has exited".to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(?error, "internal error");
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
                "session store unavailable".to_string(),
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
            Self::Internal(m) | Self::NotFound(m) | Self::BadRequest(m) => m.clone(),
            Self::Session(e) => format!("{e:#}"),
            Self::StoreUnavailable => "session store unavailable".to_string(),
        };
        ServerEvent::Error {
            session_id: None,
            request_id: request_id.map(str::to_string),
            message,
        }
    }
}

// ── WebSocket 处理 ────────────────────────────────────────────────────────

/// `GET /ws`：双向 WebSocket 事件流。连接后自动接收全局事件总线上的全部事件
/// （所有 session 的事件均携带 `session_id`，前端按此路由）。
async fn handle_ws(
    State(state): State<WebState>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let rx = state.inner.services.bus().subscribe();
    let shutdown = state.inner.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| ws_session(socket, state, rx, shutdown)))
}

/// WebSocket 会话：订阅全局事件总线推送给客户端；客户端命令经 [`dispatch`] 分发。
async fn ws_session(
    mut socket: WebSocket,
    state: WebState,
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
            // 服务端→客户端：全局事件总线 → 客户端
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        send_ws_response(&mut socket, &event).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(%skipped, "WebSocket client lagged, sending refresh hint");
                        send_ws_response(&mut socket, &ServerEvent::Refresh).await;
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
                                if let Some(response) = dispatch(&state, event).await {
                                    send_ws_response(&mut socket, &response).await;
                                }
                            }
                            Err(error) => {
                                tracing::warn!(?error, "failed to parse client event");
                                let msg = serde_json::json!({
                                    "type": "error",
                                    "message": format!("event parse failed: {error}")
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

/// 向 WebSocket 发送一个 ServerEvent（序列化失败发送 error 兜底）。
async fn send_ws_response(socket: &mut WebSocket, event: &ServerEvent) {
    let payload = serde_json::to_string(event)
        .unwrap_or_else(|e| format!(r#"{{"type":"error","message":"序列化失败: {e}"}}"#));
    let _ = socket.send(ws::Message::Text(payload.into())).await;
}

// ── 事件分发 ──────────────────────────────────────────────────────────────

/// 分发客户端事件到对应 handler；返回 `None` 表示无需响应（fire-and-forget）。
///
/// 命令/查询事件通过 `session_id` 路由到目标 session。
async fn dispatch(state: &WebState, event: ClientEvent) -> Option<ServerEvent> {
    let span = client_event_span(&event);
    async move {
        match event {
            // ── 查询类（携带 request_id，响应也带同一 request_id）──
            ClientEvent::GetState {
                session_id,
                request_id,
            } => Some(handlers::handle_get_state(state, &session_id, &request_id).await),
            ClientEvent::ListModels { request_id } => {
                Some(handlers::handle_list_models(state, &request_id))
            }
            ClientEvent::ListWorks { request_id } => {
                Some(handlers::handle_list_works(state, &request_id).await)
            }
            ClientEvent::ListWorkSessions {
                request_id,
                work_id,
            } => Some(handlers::handle_list_work_sessions(state, &request_id, &work_id).await),
            ClientEvent::ListProjects { request_id } => {
                Some(handlers::handle_list_projects(state, &request_id).await)
            }
            ClientEvent::ListSkills { request_id } => {
                Some(handlers::handle_list_skills(state, &request_id))
            }
            ClientEvent::ListFiles {
                session_id,
                prefix,
                request_id,
            } => Some(handlers::handle_list_files(state, &session_id, &prefix, &request_id).await),

            // ── 命令类（fire-and-forget，返回 ack 或由后续事件驱动）──
            ClientEvent::Prompt {
                session_id,
                text,
                images,
            } => Some(handlers::handle_prompt(state, &session_id, text, images).await),
            ClientEvent::UpdateQueueEntry {
                session_id,
                id,
                text,
            } => handlers::handle_update_queue_entry(state, &session_id, &id, text).await,
            ClientEvent::RemoveQueueEntry { session_id, id } => {
                handlers::handle_remove_queue_entry(state, &session_id, &id).await
            }
            ClientEvent::MoveQueueEntry {
                session_id,
                id,
                direction,
            } => handlers::handle_move_queue_entry(state, &session_id, &id, direction).await,
            ClientEvent::Cancel { session_id } => {
                Some(handlers::handle_cancel(state, &session_id).await)
            }
            ClientEvent::AnswerQuestion {
                session_id,
                id,
                answers,
                custom,
            } => Some(
                handlers::handle_answer_question(state, &session_id, id, answers, custom).await,
            ),
            ClientEvent::SwitchModel {
                session_id,
                spec,
                reasoning,
            } => Some(handlers::handle_switch_model(state, &session_id, spec, reasoning).await),
            ClientEvent::CreateWork {
                request_id,
                project,
            } => Some(handlers::handle_create_work(state, &request_id, project).await),
            ClientEvent::CreateProject { request_id, path } => {
                Some(handlers::handle_create_project(state, &request_id, path).await)
            }
            ClientEvent::DeleteWork { request_id, id } => {
                Some(handlers::handle_delete_work(state, &request_id, &id).await)
            }
            ClientEvent::RenameWork {
                request_id,
                id,
                title,
            } => Some(handlers::handle_rename_work(state, &request_id, &id, &title).await),
            ClientEvent::DeleteProject {
                request_id,
                id,
                force,
            } => Some(handlers::handle_delete_project(state, &request_id, &id, force).await),
            event @ (ClientEvent::GetSettings { .. }
            | ClientEvent::UpsertProvider { .. }
            | ClientEvent::DeleteProvider { .. }
            | ClientEvent::UpsertModelSpec { .. }
            | ClientEvent::DeleteModelSpec { .. }
            | ClientEvent::SetSetting { .. }
            | ClientEvent::UnsetSetting { .. }) => {
                Some(handlers::dispatch_settings(state, event).await)
            }
        }
    }
    .instrument(span)
    .await
}

/// 为当前客户端事件创建 tracing span，让 handler 及后续日志自动携带
/// `session_id` / `request_id`。
fn client_event_span(event: &ClientEvent) -> tracing::Span {
    match event {
        ClientEvent::GetState {
            session_id,
            request_id,
        }
        | ClientEvent::ListFiles {
            session_id,
            request_id,
            ..
        } => tracing::info_span!(
            "client_event",
            session_id = %session_id,
            request_id = %request_id
        ),
        ClientEvent::ListModels { request_id }
        | ClientEvent::ListWorks { request_id }
        | ClientEvent::ListWorkSessions { request_id, .. }
        | ClientEvent::ListProjects { request_id }
        | ClientEvent::ListSkills { request_id }
        | ClientEvent::CreateWork { request_id, .. }
        | ClientEvent::CreateProject { request_id, .. }
        | ClientEvent::DeleteProject { request_id, .. }
        | ClientEvent::DeleteWork { request_id, .. }
        | ClientEvent::RenameWork { request_id, .. }
        | ClientEvent::GetSettings { request_id }
        | ClientEvent::UpsertProvider { request_id, .. }
        | ClientEvent::DeleteProvider { request_id, .. }
        | ClientEvent::UpsertModelSpec { request_id, .. }
        | ClientEvent::DeleteModelSpec { request_id, .. }
        | ClientEvent::SetSetting { request_id, .. }
        | ClientEvent::UnsetSetting { request_id, .. } => {
            tracing::info_span!("client_event", request_id = %request_id)
        }
        ClientEvent::Prompt { session_id, .. }
        | ClientEvent::UpdateQueueEntry { session_id, .. }
        | ClientEvent::RemoveQueueEntry { session_id, .. }
        | ClientEvent::MoveQueueEntry { session_id, .. }
        | ClientEvent::Cancel { session_id }
        | ClientEvent::AnswerQuestion { session_id, .. }
        | ClientEvent::SwitchModel { session_id, .. } => {
            tracing::info_span!("client_event", session_id = %session_id)
        }
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
            tracing::warn!(%origin, "cross-origin request rejected (CSRF protection)");
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

        let state = crate::serve::tests::test_state().await;

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

    /// `get_state` 请求-响应协议：应收到携带 `request_id` 与真实
    /// `session_id` 的 `state_snapshot`。
    #[tokio::test]
    async fn get_state_returns_snapshot() {
        use axum::Router;
        use axum::routing::get;
        use futures::{SinkExt, StreamExt};

        let (state, real_id) = crate::serve::tests::test_state_with_session().await;

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

        // 发送 get_state 查询
        let cmd = serde_json::json!({
            "type": "get_state",
            "session_id": real_id,
            "request_id": "test-r1",
        });
        ws_stream
            .send(tokio_tungstenite::tungstenite::Message::Text(
                cmd.to_string().into(),
            ))
            .await
            .expect("send");

        // 读取响应，应收到 state_snapshot 事件
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(msg) = ws_stream.next().await {
                if let Ok(tokio_tungstenite::tungstenite::Message::Text(text)) = msg
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
                    && json["type"] == "state_snapshot"
                {
                    return Some(json);
                }
            }
            None
        })
        .await;

        server.abort();
        let json = result
            .expect("timeout")
            .expect("state_snapshot not received");
        assert_eq!(json["request_id"], "test-r1", "request_id 应匹配");
        assert_eq!(
            json["session_id"].as_str().unwrap(),
            real_id,
            "session_id 应为真实 id"
        );
        assert!(json["snapshot"].is_object(), "快照应存在");
    }

    /// 全局事件总线：新建 session 的事件无需订阅即可到达已连接的客户端。
    #[tokio::test]
    async fn events_from_any_session_reach_client() {
        use axum::Router;
        use axum::routing::get;
        use futures::{SinkExt, StreamExt};

        let (state, session_id) = crate::serve::tests::test_state_with_session().await;

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

        // 直接向全局总线发送一个其他 session 的事件（模拟任意 session 的生命周期事件）
        let other_session_id = "some-other-session";
        state
            .inner
            .services
            .bus()
            .publish(crate::serve::ServerEvent::RunStarted {
                session_id: other_session_id.to_string(),
            });

        // 客户端应收到该事件（无需任何订阅动作）
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(msg) = ws_stream.next().await {
                if let Ok(tokio_tungstenite::tungstenite::Message::Text(text)) = msg
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
                    && json["type"] == "run_started"
                {
                    return Some(json);
                }
            }
            None
        })
        .await;

        server.abort();
        let json = result.expect("timeout").expect("run_started not received");
        assert_eq!(
            json["session_id"].as_str().unwrap(),
            other_session_id,
            "事件应携带源 session id"
        );

        // prompt 命令的 ack 也应通过同一连接返回（命令路径回归）
        let cmd = serde_json::json!({
            "type": "cancel",
            "session_id": session_id,
        });
        ws_stream
            .send(tokio_tungstenite::tungstenite::Message::Text(
                cmd.to_string().into(),
            ))
            .await
            .expect("send cancel");
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
