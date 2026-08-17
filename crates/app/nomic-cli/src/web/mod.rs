//! web 模式（`--web`）：内置 HTTP 服务（axum）+ SSE 事件流 + 前端静态伺服。
//!
//! 架构见 `docs/adr/0030-web-ui.md`。复用 `bootstrap` 运行时与 print/TUI 的
//! 事件落库 seam（[`SessionRecorder`] 一行接线）；agent actor（ADR-0022）经
//! [`nomic_core::AgentHandle`] 驱动，事件经 broadcast 分发给全部 SSE 客户端。
//!
//! 运行调度对齐 TUI 的统一消息队列：运行中提交的 prompt 入队，当前轮完成后
//! 按序续跑（[`RunGate`] 把「入队 + 抢 running 标志」与 runner 的「出队 +
//! 复位」收在同一把锁下，杜绝空队列与复位之间的丢单竞态）。

mod api;
mod assets;
mod question;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use nomic_ai::{ImageContent, Message, Model, ThinkingLevel};
use nomic_core::{Agent, AgentEvent};
use nomic_session::SessionRecorder;
use nomic_tools::{AskUserAnswer, AskUserQuestion, TodoStore};
use serde::Serialize;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::bootstrap::{self, Bootstrap};
use crate::model::ModelResolver;
use crate::{Cli, web::question::WebQuestionSink};

/// 服务端推送给前端的事件（SSE 负载；`type` 字段区分事件种类）。
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

/// 运行时共享状态（全部可变部分收在 `Runtime`；axum 状态经 [`AppState`] 注入）。
///
/// 手动实现 Debug（`ModelResolver` 不实现 Debug，跳过该字段）。
pub struct Runtime {
    /// agent 事件广播通道（前端 SSE 订阅源）
    events: broadcast::Sender<ServerEvent>,
    /// 运行时模型解析器（候选列表与切换，与启动同一分层口径）
    models: Arc<ModelResolver>,
    /// session 落库槽（库不可用时为 `None`；事件转发与新建/恢复共用一把锁）
    recorder: Mutex<Option<SessionRecorder>>,
    /// 提问应答表（question id → 回答通道；断线重连经状态快照恢复弹层）
    questions: Mutex<HashMap<String, PendingQuestion>>,
    /// prompt 队列 + 运行标志（见 [`RunGate`]）
    gate: RunGate,
    /// 当前轮的取消令牌（`POST /api/cancel` 取消用）
    cancel: Mutex<Option<CancellationToken>>,
    /// 服务停机令牌：退出时取消，结束全部 SSE 长连接（graceful shutdown
    /// 会等所有在途连接结束，SSE 不主动断的话 serve 永不返回）
    shutdown: CancellationToken,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("events", &self.events)
            .field("models", &"<ModelResolver>")
            .field("recorder", &self.recorder)
            .field("questions", &self.questions)
            .field("gate", &self.gate)
            .field("cancel", &self.cancel)
            .field("shutdown", &self.shutdown)
            .finish_non_exhaustive()
    }
}

/// axum 路由状态：运行时 + 固定 agent 句柄（可克隆）。
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Runtime>,
    pub handle: nomic_core::AgentHandle,
}

/// 进入 web 模式：bootstrap 装配运行时 → 构建 agent actor → 启动事件转发 →
/// 起 HTTP 服务（REST + SSE + 静态前端）。
pub async fn run(cli: &Cli) -> Result<()> {
    let boot = bootstrap::bootstrap(cli).await?;
    let state = build_app_state(boot);

    let app = api::router(state.clone());
    let host = cli.host.as_deref().unwrap_or(DEFAULT_HOST);
    let listener = tokio::net::TcpListener::bind((host, cli.port))
        .await
        .with_context(|| format!("绑定 {}:{} 失败（--host/--port 可调整）", host, cli.port))?;
    let local = listener.local_addr().context("读取监听地址失败")?;
    println!("\x1b[36m▸ nomic web UI: http://{local}\x1b[0m");
    println!(
        "\x1b[2m  cwd: {} · 模型: {} · 前端: 内嵌（web/dist 编译期打包）\x1b[0m",
        std::env::current_dir().map_or_else(|_| "?".into(), |p| p.display().to_string()),
        state.handle.model().await.map_or_else(
            |_| "?".to_string(),
            |model| format!("{}/{}", model.provider, model.id),
        ),
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

/// 构建共享状态：runtime（提问表/队列/广播）+ agent actor + 事件转发任务。
fn build_app_state(boot: Bootstrap) -> AppState {
    let (events_tx, _) = broadcast::channel::<ServerEvent>(512);
    let recorder = boot
        .session
        .map(|(store, id)| SessionRecorder::new(store, id));
    let runtime = Arc::new(Runtime {
        events: events_tx,
        models: Arc::new(boot.models),
        recorder: Mutex::new(recorder),
        questions: Mutex::new(HashMap::new()),
        gate: RunGate {
            queue: Mutex::new(VecDeque::new()),
            running: AtomicBool::new(false),
        },
        cancel: Mutex::new(None),
        shutdown: CancellationToken::new(),
    });

    let sink = Arc::new(WebQuestionSink {
        runtime: runtime.clone(),
    });
    let (agent, events_rx) = Agent::builder()
        .model(boot.model)
        .provider(boot.provider)
        .system_prompt(boot.system_prompt)
        .tools(nomic_tools::default_tools_with_skills(
            boot.skill_resolver,
            TodoStore::new(),
            sink,
        ))
        .messages(boot.history)
        .stream_options(boot.stream_options)
        .compaction(boot.compaction)
        .build();
    let (handle, _actor_task) = agent.spawn();

    // 事件转发：落库（同一 seam）+ 广播给全部 SSE 客户端
    tokio::spawn(forward_events(runtime.clone(), events_rx));

    AppState {
        inner: runtime,
        handle,
    }
}

/// 事件转发任务：消费 agent 事件流，先经 [`SessionRecorder`] 落库（定稿点，
/// 失败仅告警，与 print/TUI 同一口径），再广播给全部 SSE 客户端。
async fn forward_events(runtime: Arc<Runtime>, mut events: mpsc::UnboundedReceiver<AgentEvent>) {
    while let Some(event) = events.recv().await {
        let mut recorder = runtime.recorder.lock().await;
        if let Some(recorder) = &mut *recorder
            && let Err(error) = recorder.record(&event).await
        {
            tracing::warn!(%error, "session 落库失败");
        }
        drop(recorder);

        // 运行生命周期事件由 AgentStart/AgentEnd 翻译产出：转发任务是广播的
        // 唯一发送方，保证 run 状态与 agent 事件顺序一致（run_loop 直接广播
        // 会与尾部事件竞态）。无订阅者（广播 Err）时忽略，快照接口兜底。
        if matches!(event, AgentEvent::AgentStart) {
            let _ = runtime.events.send(ServerEvent::RunStarted);
        } else if matches!(event, AgentEvent::AgentEnd { .. }) {
            let _ = runtime.events.send(ServerEvent::RunFinished);
        }
        let _ = runtime.events.send(ServerEvent::Agent { event });
    }
}

/// 提交 prompt：空闲时启动 runner 任务，运行中入队（对齐 TUI 统一消息队列
/// 语义——当前轮完成后按序续跑）。返回 `true` 表示本轮立即启动。
pub async fn submit_prompt(state: &AppState, text: String, images: Vec<ImageContent>) -> bool {
    let started = state
        .inner
        .gate
        .submit(PendingPrompt { text, images })
        .await;
    if started {
        let state = state.clone();
        tokio::spawn(async move {
            run_loop(&state).await;
        });
    }
    started
}

/// runner 任务：串行消费队列；每轮 prompt 带独立取消令牌，可被
/// `POST /api/cancel` 单独中断（中断后队列保留，恢复后继续下一轮）。
async fn run_loop(state: &AppState) {
    while let Some(prompt) = state.inner.gate.next().await {
        let cancel = CancellationToken::new();
        *state.inner.cancel.lock().await = Some(cancel.clone());
        let result = state
            .handle
            .prompt_with_images(&prompt.text, &prompt.images, cancel.clone())
            .await;
        *state.inner.cancel.lock().await = None;
        if let Err(error) = result {
            tracing::error!(%error, "agent run failed");
            let _ = state.inner.events.send(ServerEvent::Error {
                message: format!("{error:#}"),
            });
            // agent loop 整体失败时无 AgentEnd 事件（见 forward_events），
            // 补发 RunFinished 避免前端运行状态悬挂
            let _ = state.inner.events.send(ServerEvent::RunFinished);
        }
    }
}

/// 取消当前轮运行；没有进行中的运行时返回 `false`（前端据此提示）。
pub async fn cancel_run(state: &AppState) -> bool {
    state
        .inner
        .cancel
        .lock()
        .await
        .take()
        .is_some_and(|cancel| {
            cancel.cancel();
            true
        })
}

/// 回答一个提问：从应答表取出通道并回填；提问不存在或已被取消返回 `false`。
pub async fn answer_question(state: &AppState, id: &str, answer: AskUserAnswer) -> bool {
    let Some(pending) = state.inner.questions.lock().await.remove(id) else {
        return false;
    };
    pending.answer.send(answer).is_ok()
}

/// 当前状态快照的各部分（api 层拼装成响应）。
pub struct Snapshot {
    pub messages: Vec<Message>,
    pub model: Model,
    pub reasoning: Option<ThinkingLevel>,
    pub context_tokens: u64,
    pub running: bool,
    pub queued: usize,
    pub session: Option<(String, Option<String>)>,
    pub pending_question: Option<(String, AskUserQuestion)>,
    pub cwd: PathBuf,
}

/// 收集当前状态快照：agent 查询（经 actor 邮箱）+ 运行时可变状态。
pub async fn snapshot(state: &AppState) -> Result<Snapshot> {
    let messages = state.handle.messages().await?;
    let model = state.handle.model().await?;
    let reasoning = state.handle.reasoning().await?;
    let context_tokens = state.handle.context_tokens().await?;
    let (running, queued) = (state.inner.gate.running(), state.inner.gate.len().await);
    let session = {
        let recorder = state.inner.recorder.lock().await;
        recorder.as_ref().map(|recorder| {
            let title = nomic_session::session_title(&messages);
            (recorder.session_id().to_string(), title)
        })
    };
    let pending_question = state
        .inner
        .questions
        .lock()
        .await
        .iter()
        .next()
        .map(|(id, pending)| (id.clone(), pending.question.clone()));
    let cwd = std::env::current_dir().context("get cwd")?;
    Ok(Snapshot {
        messages,
        model,
        reasoning,
        context_tokens,
        running,
        queued,
        session,
        pending_question,
        cwd,
    })
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
                let _ = cancel_run(&state).await;
            }
        }
        _ = &mut quit_rx => {
            tracing::info!("收到退出键，取消运行并关闭服务");
            let _ = cancel_run(&state).await;
        }
    }
    // 断掉全部 SSE 长连接：graceful shutdown 会等所有在途连接结束，SSE 流
    // 由前端持续持有，不主动结束的话 serve 永不返回、进程挂住。
    state.inner.shutdown.cancel();
    stop.cancel();
    let _ = keyboard.await;
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

    use super::*;

    /// 构建一个最小 AppState（内存 session 库 + 空 agent 构建）。
    pub(super) async fn test_state() -> AppState {
        let store = nomic_session::SessionStore::in_memory()
            .await
            .expect("store");
        let id = store.create_session(".").await.expect("session");
        let (events_tx, _) = broadcast::channel(16);
        let runtime = Arc::new(Runtime {
            events: events_tx,
            models: Arc::new(ModelResolver::new(
                &Cli::parse_from(["nomic", "--model", "openai/gpt-4o"]),
                None,
                None,
                None,
            )),
            recorder: Mutex::new(Some(SessionRecorder::new(store, id))),
            questions: Mutex::new(HashMap::new()),
            gate: RunGate {
                queue: Mutex::new(VecDeque::new()),
                running: AtomicBool::new(false),
            },
            cancel: Mutex::new(None),
            shutdown: CancellationToken::new(),
        });
        let sink = Arc::new(WebQuestionSink {
            runtime: runtime.clone(),
        });
        let (agent, _events) = Agent::builder()
            .model(Model {
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
            })
            .provider(crate::model::build_provider(
                nomic_ai::ApiKind::OpenAiCompletions,
                Some("sk-test".into()),
            ))
            .system_prompt("test")
            .tools(nomic_tools::default_tools_with_skills(
                nomic_skills::SkillResolver::new(
                    Path::new("/repo"),
                    nomic_skills::ProjectDiscovery::Roots(Vec::new()),
                    Vec::new(),
                )
                .expect("skills"),
                TodoStore::new(),
                sink,
            ))
            .build();
        let (handle, _actor_task) = agent.spawn();
        AppState {
            inner: runtime,
            handle,
        }
    }

    #[tokio::test]
    async fn run_gate_queues_while_running_and_resets_when_empty() {
        let gate = RunGate {
            queue: Mutex::new(VecDeque::new()),
            running: AtomicBool::new(false),
        };
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
        assert!(!cancel_run(&state).await, "空闲时取消应返回 false");
    }

    #[tokio::test]
    async fn answer_question_roundtrip() {
        let state = test_state().await;
        let (tx, rx) = oneshot::channel();
        state.inner.questions.lock().await.insert(
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
        assert!(answer_question(&state, "q1", answer.clone()).await);
        assert_eq!(rx.await.expect("answer"), answer);
        assert!(
            !answer_question(&state, "q1", answer.clone()).await,
            "重复回答应失败"
        );
        assert!(!answer_question(&state, "missing", answer).await);
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
        let snap = snapshot(&state).await.expect("snapshot");
        assert!(snap.messages.is_empty());
        assert_eq!(snap.model.provider, "openai");
        assert_eq!(snap.model.id, "gpt-4o");
        assert!(!snap.running);
        assert_eq!(snap.queued, 0);
        assert!(snap.session.is_some(), "内存库 session 应存在");
        assert!(snap.pending_question.is_none());
    }
}
