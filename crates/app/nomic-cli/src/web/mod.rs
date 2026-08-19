//! web 模式（`--web`）：内置 HTTP 服务（axum）+ SSE 事件流 + 前端静态伺服。
//!
//! 架构见 `docs/adr/0030-web-ui.md`。复用 `bootstrap` 运行时与 print/TUI 的
//! 事件落库 seam（[`SessionRecorder`] 一行接线）；agent actor（ADR-0022）经
//! [`nomic_core::AgentHandle`] 驱动。
//!
//! 多 session 并行：进程级 [`Runtime`] 持有一个 session 注册表
//! （`id → SessionRuntime`），每个 [`SessionRuntime`] 自持一个 agent actor、
//! 独立的 prompt 队列 / 取消令牌 / 事件广播 / 落库器——多个 session 的
//! runner 任务由 tokio 多线程运行时天然并行，互不阻塞。模型选择按 session
//! 隔离并持久化到 sqlite 会话级 config（见 nomic-session 迁移 0004）。

mod api;
mod assets;
mod question;
mod session;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use nomic_ai::ImageContent;
use nomic_core::AgentEvent;
use nomic_session::{SessionRecorder, SessionStore};
use nomic_tools::{AskUserAnswer, AskUserQuestion};
use serde::Serialize;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::bootstrap::{self, Bootstrap};
use crate::model::ModelResolver;
use crate::{Cli, web::api::ApiError};
use session::{ResolvedSessionModel, SessionFactory};

pub use session::{Snapshot, snapshot};

/// 服务端推送给前端的事件（SSE 负载；`type` 字段区分事件种类）。
///
/// 事件经每个 session 各自的 broadcast 分发（见 [`SessionRuntime::events`]），
/// 客户端按 session 订阅，故负载无需再带 session id。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// agent 生命周期事件（原样透传，前端按既有事件协议重建消息流）
    Agent { event: AgentEvent },
    /// `ask_user_question` 提问（前端弹层展示，回答经 REST 回填）
    Question {
        id: String,
        question: AskUserQuestion,
    },
    /// 提问被取消（运行中断）
    QuestionCancelled { id: String },
    /// 一轮 run 开始（runner 任务启动）
    RunStarted,
    /// 一轮 run 结束（队列清空或出错）
    RunFinished,
    /// 运行期错误（agent loop 失败等）
    Error { message: String },
}

/// 待发送的 prompt（文本 + 图片附件）。
#[derive(Debug)]
pub struct PendingPrompt {
    pub text: String,
    pub images: Vec<ImageContent>,
}

/// 进行中的提问（应答表条目）：问题内容供状态快照重放（前端断线重连后
/// 弹层恢复），oneshot 在回答到达时回填给 `ask_user_question` 工具。
#[derive(Debug)]
pub struct PendingQuestion {
    pub question: AskUserQuestion,
    answer: oneshot::Sender<AskUserAnswer>,
}

/// 运行门：prompt 队列 + 运行标志，同一把锁维护。
///
/// `submit` 的「入队 + 读标志 + 抢标志」与 `next` 的「出队 + 复位标志」互斥，
/// 不存在「出队发现队列空但 submit 已入队」的丢单竞态；`submit` 返回 `true`
/// 表示调用方应启动 runner（空闲时抢到标志）。
#[derive(Debug)]
pub struct RunGate {
    queue: Mutex<VecDeque<PendingPrompt>>,
    running: AtomicBool,
}

impl RunGate {
    /// 空队列、未运行的门。
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            running: AtomicBool::new(false),
        }
    }

    /// 提交 prompt；返回 `true` 表示调用方应启动 runner 任务。
    pub async fn submit(&self, prompt: PendingPrompt) -> bool {
        let mut queue = self.queue.lock().await;
        queue.push_back(prompt);
        let was_running = self.running.load(Ordering::SeqCst);
        if !was_running {
            self.running.store(true, Ordering::SeqCst);
        }
        drop(queue);
        !was_running
    }

    /// 出队下一条 prompt；队列空时复位运行标志并返回 `None`（runner 应退出）。
    pub async fn next(&self) -> Option<PendingPrompt> {
        let mut queue = self.queue.lock().await;
        let next = queue.pop_front();
        if next.is_none() {
            self.running.store(false, Ordering::SeqCst);
        }
        drop(queue);
        next
    }

    /// 当前队列长度（状态快照用）。
    pub async fn len(&self) -> usize {
        self.queue.lock().await.len()
    }

    /// 是否有 runner 任务在跑（状态快照用）。
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Default for RunGate {
    fn default() -> Self {
        Self::new()
    }
}

/// 单个 session 的运行时：自持 agent actor、prompt 队列、取消令牌、
/// 事件广播、提问表与落库器。并行执行的单位。
#[derive(Debug)]
pub struct SessionRuntime {
    /// session id（registry 键；store 不可用时为临时 UUID）
    pub id: String,
    /// agent actor 句柄（本 session 的上下文、模型、工具）
    pub handle: nomic_core::AgentHandle,
    /// 落库槽（store 不可用时为 `None`）
    pub recorder: Mutex<Option<SessionRecorder>>,
    /// 本 session 的事件广播（前端按 session 订阅）
    pub events: broadcast::Sender<ServerEvent>,
    /// prompt 队列 + 运行标志
    pub gate: RunGate,
    /// 当前轮的取消令牌
    pub cancel: Mutex<Option<CancellationToken>>,
    /// 提问应答表（question id → 回答通道）
    pub questions: Arc<Mutex<HashMap<String, PendingQuestion>>>,
}

impl SessionRuntime {
    /// 提交 prompt；空闲时启动本 session 的 runner，运行中入队。
    /// 返回 `true` 表示本轮立即启动。
    pub async fn submit_prompt(self: &Arc<Self>, text: String, images: Vec<ImageContent>) -> bool {
        let started = self.gate.submit(PendingPrompt { text, images }).await;
        if started {
            let session = self.clone();
            tokio::spawn(async move {
                session::run_loop(&session).await;
            });
        }
        started
    }

    /// 取消当前轮运行；没有进行中的运行时返回 `false`。
    pub async fn cancel_run(&self) -> bool {
        self.cancel.lock().await.take().is_some_and(|cancel| {
            cancel.cancel();
            true
        })
    }

    /// 回答一个提问：从应答表取出通道并回填；提问不存在或已被取消返回 `false`。
    pub async fn answer_question(&self, id: &str, answer: AskUserAnswer) -> bool {
        let Some(pending) = self.questions.lock().await.remove(id) else {
            return false;
        };
        pending.answer.send(answer).is_ok()
    }
}

/// 进程级运行时：session 注册表 + 共享 store / 模型解析器 / 停机令牌。
///
/// 手动实现 Debug（`ModelResolver` / `SessionFactory` 不实现 Debug，跳过）。
pub struct Runtime {
    /// session 库（不可用时为 `None`，降级为不持久化）
    pub(crate) store: Option<SessionStore>,
    /// 模型候选解析器（候选列表与 api_key 分层，进程级共享）
    pub(crate) models: Arc<ModelResolver>,
    /// session 注册表（id → 并行运行的 SessionRuntime）
    pub(crate) sessions: Mutex<HashMap<String, Arc<SessionRuntime>>>,
    /// 服务停机令牌
    pub(crate) shutdown: CancellationToken,
    /// 构建 SessionRuntime 的工厂（bootstrap 输入）
    pub(crate) factory: SessionFactory,
    /// 启动时 bootstrap 的初始 session id（前端挂载时确定默认会话）
    pub(crate) default_session_id: String,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("store", &self.store)
            .field("models", &"<ModelResolver>")
            .field("sessions", &self.sessions)
            .field("shutdown", &self.shutdown)
            .field("factory", &"<SessionFactory>")
            .field("default_session_id", &self.default_session_id)
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// 取或惰性打开一个 session：已注册直接返回；否则从 store 加载历史并
    /// 构建 SessionRuntime（服务重启后或其他未打开会话的首访）。
    pub(crate) async fn open_session(&self, id: &str) -> Result<Arc<SessionRuntime>, ApiError> {
        if let Some(session) = self.sessions.lock().await.get(id) {
            return Ok(session.clone());
        }
        let (history, tip) = match &self.store {
            Some(store) => {
                let history = match store.load_messages(id).await {
                    Ok(history) => history,
                    Err(nomic_session::SessionError::SessionNotFound(_)) => {
                        return Err(ApiError::NotFound(format!("session {id} not found")));
                    }
                    Err(error) => return Err(error.into()),
                };
                let tip = store.latest_entry_id(id).await?;
                (history, tip)
            }
            None => (Vec::new(), None),
        };
        let resolved = self
            .factory
            .resolve_session_model(self.store.as_ref(), id)
            .await;
        let session =
            self.factory
                .build(self.store.clone(), id.to_string(), history, tip, resolved);
        self.sessions
            .lock()
            .await
            .insert(id.to_string(), session.clone());
        Ok(session)
    }

    /// 新建一个 session：落库（可用时）+ 以进程默认模型构建 SessionRuntime。
    pub(crate) async fn create_session(&self) -> Result<Arc<SessionRuntime>, ApiError> {
        let cwd = std::env::current_dir().context("get cwd")?;
        let id = match &self.store {
            Some(store) => store.create_session(&cwd).await?,
            None => uuid::Uuid::now_v7().to_string(),
        };
        let resolved = self
            .factory
            .resolve_session_model(self.store.as_ref(), &id)
            .await;
        let session =
            self.factory
                .build(self.store.clone(), id.clone(), Vec::new(), None, resolved);
        self.sessions.lock().await.insert(id, session.clone());
        Ok(session)
    }

    /// 列出全部 session 摘要（store 不可用时报错）。
    pub(crate) async fn list_sessions(
        &self,
    ) -> Result<Vec<nomic_session::SessionSummary>, ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        Ok(store.list_sessions().await?)
    }
}

/// axum 路由状态：进程级运行时（可克隆）。
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Runtime>,
}

/// 进入 web 模式：bootstrap 装配运行时 → 构建初始 session → 起 HTTP 服务。
pub async fn run(cli: &Cli) -> Result<()> {
    let boot = bootstrap::bootstrap(cli).await?;
    let state = build_app_state(boot).await;

    let app = api::router(state.clone());
    let host = cli.host.as_deref().unwrap_or(DEFAULT_HOST);
    let listener = tokio::net::TcpListener::bind((host, cli.port))
        .await
        .with_context(|| format!("绑定 {}:{} 失败（--host/--port 可调整）", host, cli.port))?;
    let local = listener.local_addr().context("读取监听地址失败")?;
    println!("\x1b[36m▸ nomic web UI: http://{local}\x1b[0m");
    println!(
        "\x1b[2m  cwd: {} · 前端: 内嵌（web/dist 编译期打包）\x1b[0m",
        std::env::current_dir().map_or_else(|_| "?".into(), |p| p.display().to_string()),
    );
    tracing::info!(%local, "nomic web UI 启动");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state))
        .await
        .context("HTTP 服务异常退出")?;
    Ok(())
}

/// 缺省监听地址（仅本机；`--host` 显式覆盖，跨机访问自担风险）。
const DEFAULT_HOST: &str = "127.0.0.1";

/// 构建进程级运行时：session 注册表 + 工厂 + 初始 session。
async fn build_app_state(boot: Bootstrap) -> AppState {
    let models = Arc::new(boot.models);
    let store = boot.session.as_ref().map(|(store, _)| store.clone());
    let default_reasoning = boot.stream_options.reasoning;
    let factory = SessionFactory {
        models: models.clone(),
        system_prompt: boot.system_prompt,
        skill_resolver: boot.skill_resolver,
        stream_options: boot.stream_options,
        compaction: boot.compaction,
        default_model: boot.model.clone(),
        default_provider: boot.provider,
        default_reasoning,
        available_models: boot.available_models,
    };

    // 初始 session：bootstrap 已解析默认模型 / provider / 历史；落库 tip 取
    // 默认分支末端（分支场景下保证续写落在默认分支，与 TUI resume 同口径）。
    let (default_session_id, tip) = match &boot.session {
        Some((store, id)) => (id.clone(), store.latest_entry_id(id).await.ok().flatten()),
        None => (uuid::Uuid::now_v7().to_string(), None),
    };
    let default_stream_options = {
        let mut options = factory.stream_options.clone();
        options.reasoning = default_reasoning;
        options
    };
    let initial = factory.build(
        store.clone(),
        default_session_id.clone(),
        boot.history,
        tip,
        ResolvedSessionModel {
            model: factory.default_model.clone(),
            provider: factory.default_provider.clone(),
            options: default_stream_options,
        },
    );

    let runtime = Arc::new(Runtime {
        store,
        models,
        sessions: Mutex::new(HashMap::from([(default_session_id.clone(), initial)])),
        shutdown: CancellationToken::new(),
        factory,
        default_session_id,
    });
    AppState { inner: runtime }
}

/// 优雅退出：q 或 Ctrl+C 取消当前运行后关闭 HTTP 服务。
///
/// 键盘轮询全程开 raw mode：cooked 模式下按键被 tty 行缓冲，q 需回车才
/// 送达进程；raw mode 同时关闭 ISIG，Ctrl+C 不再产生 SIGINT，而是以
/// `Char('c') + CONTROL` 按键事件送达（见 [`is_quit_key`]）。轮询任务退出
/// 时恢复 cooked 模式。`tokio::signal::ctrl_c` 保留：raw mode 开启失败
/// （stdin 非 tty）或外部直接发 SIGINT 时兜底。
async fn shutdown_signal(state: AppState) {
    let (quit_tx, mut quit_rx) = oneshot::channel::<()>();

    // 退出令牌：停机时停掉轮询任务并等它恢复终端；spawn_blocking 任务
    // 不退出的话 runtime 关闭会一直等它，进程挂住退不出来。
    let stop = CancellationToken::new();
    let stop_keyboard = stop.clone();
    let keyboard = tokio::task::spawn_blocking(move || {
        let _raw_guard = RawModeGuard::enter();
        loop {
            if stop_keyboard.is_cancelled() {
                break;
            }
            if event::poll(std::time::Duration::from_millis(200)).unwrap_or(false)
                && let Ok(Event::Key(key)) = event::read()
                && is_quit_key(key)
            {
                let _ = quit_tx.send(());
                break;
            }
        }
    });

    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if result.is_ok() {
                tracing::info!("收到 Ctrl+C，取消运行并关闭服务");
                cancel_all(&state).await;
            }
        }
        _ = &mut quit_rx => {
            tracing::info!("收到退出键，取消运行并关闭服务");
            cancel_all(&state).await;
        }
    }
    // 断掉全部 SSE 长连接：graceful shutdown 会等所有在途连接结束，SSE 流
    // 由前端持续持有，不主动结束的话 serve 永不返回、进程挂住。
    state.inner.shutdown.cancel();
    stop.cancel();
    let _ = keyboard.await;
}

/// 停机时取消全部 session 的进行中运行（队列保留，进程即将退出）。
async fn cancel_all(state: &AppState) {
    let sessions = state.inner.sessions.lock().await;
    for session in sessions.values() {
        let _ = session.cancel_run().await;
    }
}

/// 退出键：q，或 Ctrl+C（raw mode 下 ISIG 关闭，Ctrl+C 以按键事件送达）。
fn is_quit_key(key: event::KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// 退出时恢复 cooked 模式，把 tty 还给 shell（任务 panic 也经 Drop 恢复）。
struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> Self {
        let _ = crossterm::terminal::enable_raw_mode();
        Self
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser as _;
    use nomic_ai::{Model, StreamOptions};
    use nomic_skills::SkillResolver;

    use super::*;

    /// 构建一个最小 AppState（内存 session 库 + 空 agent 构建）。
    pub(super) async fn test_state() -> AppState {
        let store = SessionStore::in_memory().await.expect("store");
        let id = store.create_session(".").await.expect("session");
        let models = Arc::new(ModelResolver::new(
            &Cli::parse_from(["nomic", "--model", "openai/gpt-4o"]),
            None,
            None,
            None,
        ));
        let model = Model {
            id: "gpt-4o".into(),
            name: "gpt-4o".into(),
            api: nomic_ai::ApiKind::OpenAiCompletions,
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            reasoning: false,
            context_window: 128_000,
            max_tokens: 4_096,
            cost_input: 0.0,
            cost_output: 0.0,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
        };
        let provider = crate::model::build_provider(
            nomic_ai::ApiKind::OpenAiCompletions,
            Some("sk-test".into()),
        );
        let factory = SessionFactory {
            models: models.clone(),
            system_prompt: "test".to_string(),
            skill_resolver: SkillResolver::new(
                Path::new("/repo"),
                nomic_skills::ProjectDiscovery::Roots(Vec::new()),
                Vec::new(),
            )
            .expect("skills"),
            stream_options: StreamOptions::default(),
            compaction: nomic_core::CompactionSettings::default(),
            default_model: model.clone(),
            default_provider: provider,
            default_reasoning: None,
            available_models: vec![model.clone()],
        };
        let initial = factory.build(
            Some(store.clone()),
            id.clone(),
            Vec::new(),
            None,
            ResolvedSessionModel {
                model,
                provider: factory.default_provider.clone(),
                options: StreamOptions::default(),
            },
        );
        let runtime = Arc::new(Runtime {
            store: Some(store),
            models,
            sessions: Mutex::new(HashMap::from([(id.clone(), initial)])),
            shutdown: CancellationToken::new(),
            factory,
            default_session_id: id,
        });
        AppState { inner: runtime }
    }

    #[tokio::test]
    async fn run_gate_queues_while_running_and_resets_when_empty() {
        let gate = RunGate::new();
        let prompt = |text: &str| PendingPrompt {
            text: text.to_string(),
            images: Vec::new(),
        };

        assert!(
            gate.submit(prompt("first")).await,
            "空闲时首提交应启动 runner"
        );
        assert!(
            !gate.submit(prompt("second")).await,
            "运行中提交应入队而非启动 runner"
        );
        assert_eq!(gate.len().await, 2);
        assert!(gate.running());

        assert_eq!(gate.next().await.expect("first").text, "first");
        assert_eq!(gate.next().await.expect("second").text, "second");
        assert!(gate.next().await.is_none(), "队列空应复位运行标志");
        assert!(!gate.running());

        // 复位后再次提交重新启动（空队列与复位之间不丢单）
        assert!(gate.submit(prompt("third")).await);
        assert_eq!(gate.next().await.expect("third").text, "third");
    }

    #[tokio::test]
    async fn cancel_run_returns_false_when_idle() {
        let state = test_state().await;
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .values()
            .next()
            .expect("session")
            .clone();
        assert!(!session.cancel_run().await, "空闲时取消应返回 false");
    }

    #[tokio::test]
    async fn answer_question_roundtrip() {
        let state = test_state().await;
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .values()
            .next()
            .expect("session")
            .clone();
        let (tx, rx) = oneshot::channel();
        session.questions.lock().await.insert(
            "q1".to_string(),
            PendingQuestion {
                question: AskUserQuestion {
                    question: "继续？".to_string(),
                    kind: nomic_tools::QuestionKind::SingleChoice,
                    options: vec!["是".to_string(), "否".to_string()],
                },
                answer: tx,
            },
        );
        let answer = AskUserAnswer {
            answers: vec!["是".to_string()],
            custom: None,
        };
        assert!(session.answer_question("q1", answer.clone()).await);
        assert_eq!(rx.await.expect("answer"), answer);
        assert!(
            !session.answer_question("q1", answer.clone()).await,
            "重复回答应失败"
        );
        assert!(!session.answer_question("missing", answer).await);
    }

    #[test]
    fn is_quit_key_matches_q_and_ctrl_c() {
        let key = |code, modifiers| event::KeyEvent::new(code, modifiers);
        assert!(is_quit_key(key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(is_quit_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!is_quit_key(key(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert!(!is_quit_key(key(KeyCode::Char('Q'), KeyModifiers::NONE)));
        assert!(!is_quit_key(key(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[tokio::test]
    async fn snapshot_reports_state() {
        let state = test_state().await;
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .values()
            .next()
            .expect("session")
            .clone();
        let snap = snapshot(&session).await.expect("snapshot");
        assert!(snap.messages.is_empty());
        assert_eq!(snap.model.provider, "openai");
        assert_eq!(snap.model.id, "gpt-4o");
        assert!(!snap.running);
        assert_eq!(snap.queued, 0);
        assert!(snap.session.is_some(), "内存库 session 应存在");
        assert!(snap.pending_question.is_none());
    }

    #[tokio::test]
    async fn create_session_registers_independent_runtime() {
        let state = test_state().await;
        let created = state.inner.create_session().await.expect("create session");
        assert_eq!(
            state.inner.sessions.lock().await.len(),
            2,
            "新 session 应注册进表"
        );
        assert!(state.inner.sessions.lock().await.contains_key(&created.id));
    }
}
