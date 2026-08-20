//! web 模式的 HTTP 层（axum）：纯 WebSocket 事件驱动 + 静态前端伺服。
//!
//! 所有前端↔后端通信均通过 `ws://{host}/ws` 双向事件流：
//! - **客户端→服务端**：`ClientEvent`（JSON text frame，`type` 字段区分事件种类）
//! - **服务端→客户端**：`ServerEvent`（JSON text frame，`type` 字段区分事件种类）
//!
//! 事件架构：进程级全局事件总线（[`crate::web::Runtime::events`]），所有 session 的生命周期
//! 事件直接发往总线，每个事件携带 `session_id` 供前端路由。WebSocket 连接只需
//! 订阅总线一次，即可接收全部 session 的事件——无需订阅管理。
//!
//! 查询类事件（`get_state` / `list_models` / `list_sessions`）携带 `request_id`
//! 实现请求-响应关联；命令类事件（`prompt` / `cancel` 等）携带 `session_id`
//! 指定目标 session，fire-and-forget，由服务端后续事件驱动前端状态。
//!
//! 安全：缺省只绑定 `127.0.0.1`（`--host` 显式覆盖）；WebSocket 连接校验
//! `Origin` 头——非空且 host 不在本机集合、也不等于请求 `Host` 时拒绝
//! （DNS rebinding / 跨站请求防护，本服务能执行 bash）。
//! 不开放 CORS，开发期前端经 Vite 代理 `/ws` 同源访问。

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{self, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{Method, StatusCode, Uri, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::web::{AppState, ServerEvent, assets};

mod handlers;

pub use handlers::SnapshotView;

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
    /// 列出全部 session 摘要。
    ListSessions { request_id: String },
    /// 提交 prompt（空闲即跑，运行中入队）。
    Prompt {
        session_id: String,
        text: String,
        #[serde(default)]
        images: Vec<nomic_ai::ImageContent>,
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
    /// 新建 session（命令类，返回 ack 事件 `session_created`）。
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
            Self::Internal(m) | Self::NotFound(m) | Self::BadRequest(m) => m.clone(),
            Self::Session(e) => format!("{e:#}"),
            Self::StoreUnavailable => "session 库不可用".to_string(),
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
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let rx = state.inner.events.subscribe();
    let shutdown = state.inner.shutdown.clone();
    Ok(ws.on_upgrade(move |socket| ws_session(socket, state, rx, shutdown)))
}

/// WebSocket 会话：订阅全局事件总线推送给客户端；客户端命令经 [`dispatch`] 分发。
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
            // 服务端→客户端：全局事件总线 → 客户端
            result = rx.recv() => {
                match result {
                    Ok(event) => {
                        send_ws_response(&mut socket, &event).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(%skipped, "WebSocket 客户端落后，发送刷新提示");
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

/// 向 WebSocket 发送一个 ServerEvent（序列化失败发送 error 兜底）。
async fn send_ws_response(socket: &mut WebSocket, event: &ServerEvent) {
    let payload = serde_json::to_string(event)
        .unwrap_or_else(|e| format!(r#"{{"type":"error","message":"序列化失败: {e}"}}"#));
    let _ = socket.send(ws::Message::Text(payload.into())).await;
}

// ── 事件分发 ──────────────────────────────────────────────────────────────

/// 分发客户端事件到对应 handler；返回 `None` 表示无需响应（fire-and-forget）。
///
/// 命令/查询事件通过 `session_id` 路由到目标 session（`"default"` 别名自动解析）。
async fn dispatch(state: &AppState, event: ClientEvent) -> Option<ServerEvent> {
    match event {
        // ── 查询类（携带 request_id，响应也带同一 request_id）──
        ClientEvent::GetState {
            session_id,
            request_id,
        } => Some(handlers::handle_get_state(state, &session_id, &request_id).await),
        ClientEvent::ListModels { request_id } => {
            Some(handlers::handle_list_models(state, &request_id))
        }
        ClientEvent::ListSessions { request_id } => {
            Some(handlers::handle_list_sessions(state, &request_id).await)
        }

        // ── 命令类（fire-and-forget，返回 ack 或由后续事件驱动）──
        ClientEvent::Prompt {
            session_id,
            text,
            images,
        } => Some(handlers::handle_prompt(state, &session_id, text, images).await),
        ClientEvent::Cancel { session_id } => {
            Some(handlers::handle_cancel(state, &session_id).await)
        }
        ClientEvent::AnswerQuestion {
            session_id,
            id,
            answers,
            custom,
        } => Some(handlers::handle_answer_question(state, &session_id, id, answers, custom).await),
        ClientEvent::SwitchModel {
            session_id,
            spec,
            reasoning,
        } => Some(handlers::handle_switch_model(state, &session_id, spec, reasoning).await),
        ClientEvent::CreateSession => Some(handlers::handle_create_session(state).await),
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

    /// `get_state` 请求-响应协议：发送 `"default"` 别名，应收到携带
    /// `request_id` 和解析后真实 `session_id` 的 `state_snapshot`。
    #[tokio::test]
    async fn get_state_resolves_default_alias() {
        use axum::Router;
        use axum::routing::get;
        use futures::{SinkExt, StreamExt};

        let state = crate::web::tests::test_state().await;
        let real_id = state.inner.default_session_id.clone();

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
            "session_id": "default",
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
            "session_id 应为解析后的真实 id（非 \"default\"）"
        );
        assert!(json["snapshot"].is_object(), "快照应存在");
    }

    /// 全局事件总线：新建 session 的事件无需订阅即可到达已连接的客户端。
    #[tokio::test]
    async fn events_from_any_session_reach_client() {
        use axum::Router;
        use axum::routing::get;
        use futures::{SinkExt, StreamExt};

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

        // 直接向全局总线发送一个其他 session 的事件（模拟任意 session 的生命周期事件）
        let other_session_id = "some-other-session";
        state
            .inner
            .events
            .send(crate::web::ServerEvent::RunStarted {
                session_id: other_session_id.to_string(),
            })
            .expect("send to bus");

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
            "session_id": state.inner.default_session_id,
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
