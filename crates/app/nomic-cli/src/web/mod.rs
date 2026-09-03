//! web 模式（`--web`）：内置 HTTP 服务（axum）+ 纯 WebSocket 事件驱动 + 前端静态伺服。
//!
//! 架构见 `docs/adr/0030-web-ui.md`。复用 `bootstrap` 运行时与 print/TUI 的
//! 事件落库 seam（[`SessionRecorder`] 一行接线）；agent actor（ADR-0022）经
//! [`nomic_core::AgentHandle`] 驱动。
//!
//! 前端↔后端通信全部通过 `ws://{host}/ws` 双向事件流：
//! - 客户端→服务端：[`api::ClientEvent`]（`type` 字段区分事件种类）
//! - 服务端→客户端：[`ServerEvent`]（`type` 字段区分事件种类）
//!
//! 单个 WebSocket 连接可同时订阅多个 session 的事件流（`SubscribeSession`），
//! 所有事件携带 `session_id` 供前端路由到对应 session。查询类事件通过
//! `request_id` 实现请求-响应关联；命令类事件携带 `session_id` 指定目标 session，
//! 为 fire-and-forget，由服务端后续生命周期事件驱动前端状态。REST 接口已全部移除。
//!
//! 多 session 并行：进程级 [`Runtime`] 持有一个 session 注册表
//! （`id → SessionRuntime`），每个 [`SessionRuntime`] 自持一个 agent actor、
//! session runner（串行 job 队列 / 取消，ADR-0033）、事件广播与落库
//! 器——多个 session 的 runner 任务由 tokio 多线程运行时天然并行，
//! 互不阻塞。模型选择按 session 隔离并持久化到 sqlite 会话级 config
//! （见 nomic-session 迁移 0004）。

mod api;
mod assets;
mod project;
mod question;
mod queue;
mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
#[cfg(unix)]
use nix::sys::termios::{
    LocalFlags, SetArg, SpecialCharacterIndices, Termios, tcgetattr, tcsetattr,
};
use nomic_core::{AgentEvent, SessionRunner};
use nomic_session::{SessionRecorder, SessionStore};
use nomic_tools::{AskUserAnswer, AskUserQuestion, QuestionRegistry};
use serde::Serialize;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::bootstrap::{self, Bootstrap};
use crate::model::ModelResolver;
use crate::{Cli, web::api::ApiError};
use session::SessionFactory;

pub use queue::{MessageQueue, QueueEntryView};
pub use session::{Snapshot, snapshot};

/// 服务端推送给前端的事件（WebSocket text frame 负载；`type` 字段区分事件种类）。
///
/// 分为两类：
/// - **生命周期事件**（`Agent` / `RunStarted` / `RunFinished` 等）：由 agent 运行驱动，
///   经 broadcast 分发给所有客户端。
/// - **响应事件**（`StateSnapshot` / `ModelsList` 等）：由客户端查询请求触发，
///   携带 `request_id` 供客户端关联。命令类操作（`Prompt` / `Cancel`）返回 ack。
///
/// 所有事件携带 `session_id`，前端按此字段路由到对应的 session 状态。
/// 单个 WebSocket 连接自动接收所有已注册 session 的事件流。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    // ── 生命周期事件（agent 运行驱动）────────────────────────────────
    /// agent 生命周期事件（原样透传，前端按既有事件协议重建消息流）
    Agent {
        session_id: String,
        event: AgentEvent,
    },
    /// `ask_user_question` 提问（前端弹层展示，回答经事件回填）
    Question {
        session_id: String,
        id: String,
        question: AskUserQuestion,
    },
    /// 提问被取消（运行中断）
    QuestionCancelled { session_id: String, id: String },
    /// 一轮 run 开始（runner 任务启动）
    RunStarted { session_id: String },
    /// 一轮 run 结束（队列清空或出错）
    RunFinished { session_id: String },
    /// 运行期错误（agent loop 失败等）
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        message: String,
    },
    /// 客户端落后于事件流，应重新拉取快照
    Refresh,
    /// steering 队列变化（全量快照；入队 / turn 边界注入弹出 / 编辑 /
    /// 删除 / 换位后广播，前端整体替换本地队列状态）
    QueueChanged {
        session_id: String,
        queue: Vec<QueueEntryView>,
    },
    /// goal 状态变化（`/goal <目标>` 启动 / 目标完成 / 取消；前端徽标用，
    /// 断线重连后的状态以快照的 `goal` 字段为准）
    GoalChanged {
        session_id: String,
        status: GoalStatus,
        /// 目标原文（仅 `started` 携带）
        #[serde(skip_serializing_if = "Option::is_none")]
        objective: Option<String>,
    },

    // ── 查询响应事件（携带 request_id）───────────────────────────────
    /// 会话快照响应（`get_state` 查询的回复）
    StateSnapshot {
        session_id: String,
        request_id: String,
        snapshot: Box<api::SnapshotView>,
    },
    /// 候选模型列表响应（`list_models` 查询的回复）
    ModelsList {
        request_id: String,
        candidates: Vec<crate::model::ModelChoice>,
    },
    /// 全部 work 摘要响应（`list_works` 查询的回复；work 是侧栏一等入口）
    WorksList {
        request_id: String,
        works: Vec<nomic_session::WorkSummary>,
    },
    /// 一个 work 下的 session 列表响应（`list_work_sessions` 查询的回复；
    /// 含子 agent session，`parent_session_id` 记血缘，ADR-0044）
    WorkSessionsList {
        request_id: String,
        work_id: String,
        sessions: Vec<nomic_session::SessionSummary>,
    },
    /// 全部 project 摘要响应（`list_projects` 查询的回复）
    ProjectsList {
        request_id: String,
        projects: Vec<nomic_session::ProjectSummary>,
    },
    /// skill 清单响应（`list_skills` 查询的回复；`@skill://` 补全用）
    SkillsList {
        request_id: String,
        skills: Vec<api::SkillItem>,
    },
    /// 文件候选响应（`list_files` 查询的回复；`@file:` 补全用）
    FilesList {
        request_id: String,
        files: Vec<String>,
    },

    // ── 命令 ack 事件 ──────────────────────────────────────────────
    /// prompt 提交确认（`queued: true` 表示排队，`false` 表示立即运行）
    PromptAck { session_id: String, queued: bool },
    /// 取消确认
    CancelAck { session_id: String },
    /// 提问回答确认
    AnswerAck { session_id: String },
    /// 模型切换确认
    SwitchModelAck {
        session_id: String,
        choice: crate::model::ModelChoice,
    },
    /// 新建 work 确认（响应 `create_work`，携带 request_id；同时经
    /// 总线广播，其他客户端据此刷新 work 与 project 列表）。
    /// `id` 为 work id，`session_id` 为连带创建的主 session（前端打开它）
    WorkCreated {
        request_id: String,
        id: String,
        session_id: String,
    },
    /// 新建（或复用）project 确认（响应 `create_project`，携带 request_id）
    ProjectCreated {
        request_id: String,
        id: String,
        path: String,
    },
    /// 删除 session 确认（work / project 级联删除名下已打开 session 时的
    /// 广播不带 request_id）。其他客户端据此跳出被删会话的视图
    SessionDeleted {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        id: String,
    },
    /// 删除 work 确认（响应 `delete_work` 时携带 request_id；名下 session
    /// 已级联删除，被摘除的运行时另有 `session_deleted` 广播；同时经总线
    /// 广播，其他客户端据此刷新列表）
    WorkDeleted {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        id: String,
    },
    /// 重命名 work 确认（响应 `rename_work`；`title` 为生效的自定义
    /// 标题，`None` 表示已清除自定义、回退派生标题；同时经总线广播）
    WorkRenamed {
        request_id: String,
        id: String,
        title: Option<String>,
    },
    /// 删除 project 确认（响应 `delete_project`；名下 session 已级联
    /// 删除；同时经总线广播，其他客户端据此刷新列表）
    ProjectDeleted { request_id: String, id: String },
    /// 设置快照响应（`get_settings` 查询的回复；ADR-0039）
    SettingsSnapshot {
        request_id: String,
        snapshot: Box<api::SettingsSnapshotView>,
    },
    /// 设置写入确认（响应 upsert/delete/set/unset 设置命令，携带
    /// `request_id`）；成功后另有 `settings_changed` 总线广播供全部
    /// 客户端刷新
    SettingsUpdated { request_id: String },
    /// 设置已变化（无 session 维度的总线广播，同 `Refresh` 先例）：任何
    /// 设置写操作成功后发出，客户端据此重新拉取 `get_settings` 快照
    SettingsChanged,
}

/// goal 状态变化种类（`/goal <目标>` 命令驱动；serde snake_case）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// 启动 / 替换目标（携带目标原文）
    Started,
    /// 目标完成（agent 已调用 `goal_done`）
    Completed,
    /// 目标被取消（`/goal` 无参）
    Cancelled,
}

/// 单个 session 的运行时：自持 agent actor、session runner（串行 job
/// 队列与取消，ADR-0033）、事件广播、提问注册表与落库器。并行执行的单位。
#[derive(Debug)]
pub struct SessionRuntime {
    /// session id（registry 键；store 不可用时为临时 UUID）
    pub id: String,
    /// agent actor 句柄（本 session 的上下文、模型、工具）
    pub handle: nomic_core::AgentHandle,
    /// 落库槽（store 不可用时为 `None`）
    pub recorder: Mutex<Option<SessionRecorder>>,
    /// 全局事件总线（所有 session 共享；事件携带 `session_id` 区分来源）
    pub events: broadcast::Sender<ServerEvent>,
    /// run 类 job 的提交端（prompt / 压缩 / 续跑；串行消费、每 job 独立
    /// 取消令牌与生命周期翻译收在 core 的 runner）
    pub runner: SessionRunner,
    /// 在途提问注册表（与工具侧 sink 共享，nomic-tools 统一实现）：
    /// 应答回填 / 取消丢弃 / 断线重放快照的唯一口径
    pub questions: Arc<QuestionRegistry>,
    /// steering 统一消息队列（ADR-0014/0027，web 侧实现）：运行中提交的
    /// prompt 入队，core 在 turn 边界经注入点弹出；编辑按条目 id 寻址
    pub queue: MessageQueue,
    /// 本 session 的操作基准（project 严格归属）：工具相对路径以它解析，
    /// 快照展示给用户
    pub project: PathBuf,
    /// 归属信息（ADR-0044，打开时一次性查询：血缘不变）：所属 work id 与
    /// 父 session id（子 agent session；非 `None` 时本运行时只读）
    pub membership: Option<(String, Option<String>)>,
    /// 正常态工具集（goal 模式换出/换回的基准；`DynTool` 是 `Arc` 共享
    /// 句柄，克隆廉价）
    pub normal_tools: Vec<nomic_core::DynTool>,
    /// 子 agent 继承模型的共享单元（ADR-0038）：switch_model 切换本
    /// session 主 agent 模型时写入，未指定模型的子 agent 继承它
    pub inherited_model: nomic_core::SharedModel,
    /// goal 目标驱动运行的追问状态（与 `goal_done` 工具共享的会话句柄 +
    /// 连续追问计数；策略收在 nomic-tools 的 `GoalNudger`，与 TUI 同一口径）
    pub goal: std::sync::Mutex<nomic_tools::GoalNudger>,
    /// 事件转发任务句柄（删除 session 时 abort：释放任务持有的
    /// `Arc<SessionRuntime>` 引用，actor / runner 任务随后随通道关闭退出）
    tasks: std::sync::Mutex<Option<ForwardTasks>>,
}

/// 每个 session 的两个事件转发任务（agent 事件 / runner 事件）的句柄。
#[derive(Debug)]
pub struct ForwardTasks {
    pub events: tokio::task::JoinHandle<()>,
    pub runner: tokio::task::JoinHandle<()>,
}

impl SessionRuntime {
    /// 取消当前轮运行；没有进行中的运行时返回 `false`（排队 job 保留）。
    pub fn cancel_run(&self) -> bool {
        self.runner.cancel_current()
    }

    /// 回答一个提问：经注册表回填；提问不存在或已被取消返回 `false`。
    pub fn answer_question(&self, id: &str, answer: AskUserAnswer) -> bool {
        self.questions.answer(id, answer)
    }

    /// 启动 / 替换目标驱动运行（`/goal <目标>`）：创建目标会话（与
    /// `goal_done` 工具共享），换入 goal 工具集（goal_done 换入、
    /// ask_user_question 换出），把目标包装为提示词提交一轮 prompt。
    /// 调用方保证空闲（与 TUI「启动属会话命令」同一口径）且目标文本
    /// 已展开 mention。
    pub fn start_goal(&self, objective: String) -> Result<(), ApiError> {
        let session = nomic_tools::GoalSession::new(objective.clone());
        self.handle
            .set_tools(nomic_tools::goal_tools(&self.normal_tools, &session))
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        self.goal.lock().expect("goal lock").arm(session);
        self.runner
            .submit(nomic_core::SessionJob::Prompt {
                text: nomic_tools::goal_prompt(&objective),
                images: Vec::new(),
            })
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        let _ = self.events.send(ServerEvent::GoalChanged {
            session_id: self.id.clone(),
            status: GoalStatus::Started,
            objective: Some(objective),
        });
        Ok(())
    }

    /// 取消进行中的目标（`/goal` 无参）：换回正常工具集并停止自动追问。
    /// 运行中取消同样安全：追问状态立即解除（本轮结束后不再追问），工具集
    /// 经 actor 邮箱 FIFO 在本轮结束后替换。无进行中目标时返回 `false`。
    pub fn cancel_goal(&self) -> bool {
        if self.goal.lock().expect("goal lock").session().is_none() {
            return false;
        }
        self.goal.lock().expect("goal lock").disarm();
        let _ = self.handle.set_tools(self.normal_tools.clone());
        let _ = self.events.send(ServerEvent::GoalChanged {
            session_id: self.id.clone(),
            status: GoalStatus::Cancelled,
            objective: None,
        });
        true
    }

    /// 进行中的目标原文（快照携带，前端徽标与断线重放用）。
    pub fn goal_objective(&self) -> Option<String> {
        self.goal
            .lock()
            .expect("goal lock")
            .session()
            .map(|session| session.objective().to_string())
    }

    /// 关停本 session（删除时调用）：取消在途运行并 abort 事件转发任务。
    /// 转发任务与注册表是 `Arc<SessionRuntime>` 的常驻持有者；两者都释放后
    /// 本体 drop，agent actor 与 runner 任务随通道关闭自然退出。
    pub(crate) fn shutdown(&self) {
        self.cancel_run();
        let tasks = self.tasks.lock().expect("tasks lock").take();
        if let Some(tasks) = tasks {
            tasks.events.abort();
            tasks.runner.abort();
        }
    }

    /// 登记事件转发任务句柄（构建后置入，见 `SessionFactory::build`）。
    pub(crate) fn set_forward_tasks(&self, tasks: ForwardTasks) {
        *self.tasks.lock().expect("tasks lock") = Some(tasks);
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
    /// 全局事件总线：所有 session 的事件统一发往此处，WebSocket 连接订阅一次即可
    pub(crate) events: broadcast::Sender<ServerEvent>,
    /// 服务停机令牌
    pub(crate) shutdown: CancellationToken,
    /// 构建 SessionRuntime 的工厂（bootstrap 输入）
    pub(crate) factory: SessionFactory,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("store", &self.store)
            .field("models", &"<ModelResolver>")
            .field("sessions", &self.sessions)
            .field("shutdown", &self.shutdown)
            .field("factory", &"<SessionFactory>")
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// 取或惰性打开一个 session：已注册直接返回；否则从 store 加载历史并
    /// 构建 SessionRuntime（服务重启后或其他未打开会话的首访）。
    ///
    /// 新 session 插入注册表后通过 `session_added` 通知 WebSocket 连接。
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
        // project 严格归属：工具基准取 session 的 project 路径；
        // store 不可用时退回进程 cwd
        let project = match &self.store {
            Some(store) => store.session_project_path(id).await?,
            None => std::env::current_dir().context("get cwd")?,
        };
        // 归属信息（ADR-0044）：血缘不变，打开时一次性查询（快照展示与
        // 只读判定共用；查询失败等价无持久化）
        let membership = match &self.store {
            Some(store) => store.session_membership(id).await.ok().flatten(),
            None => None,
        };
        let session = self.factory.build(
            self.store.clone(),
            id.to_string(),
            history,
            project,
            resolved,
            session::SessionOpen { tip, membership },
        );
        let mut sessions = self.sessions.lock().await;
        // 并发 open 同一 id 时只保留先插入者（避免孤儿 agent 任务）
        if let Some(existing) = sessions.get(id) {
            return Ok(existing.clone());
        }
        sessions.insert(id.to_string(), session.clone());
        drop(sessions);
        Ok(session)
    }

    /// 列出全部 work 摘要（store 不可用时报错）。
    pub(crate) async fn list_works(&self) -> Result<Vec<nomic_session::WorkSummary>, ApiError> {
        let Some(store) = &self.store else {
            return Err(ApiError::StoreUnavailable);
        };
        Ok(store.list_works().await?)
    }
}

/// axum 路由状态：进程级运行时（可克隆）。
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Runtime>,
}

/// 进入 web 模式：bootstrap 装配运行时（只开 session 库，不预建 session——
/// 无默认 project，session 由前端在启动页选择 project 后显式创建）→ 起 HTTP 服务。
pub async fn run(cli: &Cli) -> Result<()> {
    let boot = bootstrap::bootstrap(cli, bootstrap::SessionPolicy::OpenStoreOnly).await?;
    let state = build_app_state(boot);

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
    tracing::info!(%local, "nomic web UI started");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state))
        .await
        .context("HTTP 服务异常退出")?;
    Ok(())
}

/// 缺省监听地址（仅本机；`--host` 显式覆盖，跨机访问自担风险）。
const DEFAULT_HOST: &str = "127.0.0.1";

/// 构建进程级运行时：session 注册表（空）+ 工厂。不预建初始 session：
/// web 模式没有默认 project，session 全部由 `create_session` 按前端选定
/// 的 project 创建，历史 session 由 `open_session` 首访时惰性构建。
fn build_app_state(boot: Bootstrap) -> AppState {
    let models = Arc::new(boot.models);
    let store = boot.store.clone();
    let default_reasoning = boot.stream_options.reasoning;
    let (events, _) = broadcast::channel::<ServerEvent>(1024);
    let factory = SessionFactory {
        models: models.clone(),
        prompt_recipe: boot.prompt_recipe,
        skill_resolver: boot.skill_resolver,
        stream_options: boot.stream_options,
        compaction: boot.compaction,
        default_model: boot.model.clone(),
        default_reasoning,
        available_models: boot.available_models,
        model_aliases: boot.model_aliases,
        events,
    };

    let runtime = Arc::new(Runtime {
        store,
        models,
        sessions: Mutex::new(HashMap::new()),
        events: factory.events.clone(),
        shutdown: CancellationToken::new(),
        factory,
    });
    AppState { inner: runtime }
}

/// 优雅退出：q 或 Ctrl+C 取消当前运行后关闭 HTTP 服务。
///
/// 键盘轮询期间仅关闭 ICANON/ECHO（见 [`QuitKeyGuard`]）：cooked 模式下
/// 按键被 tty 行缓冲，q 需回车才送达进程；关 ICANON 后按下即送达，关
/// ECHO 避免回显。OPOST/ONLCR、ISIG 等其余终端标志保持原样：web 模式
/// 不是全屏 TUI，服务存活期间仍向终端打印，不能动输出处理；Ctrl+C 仍
/// 产生 SIGINT，由 `tokio::signal::ctrl_c` 分支处理（stdin 非 tty 或外部
/// 直接发 SIGINT 时也走该分支兜底）。轮询任务退出时恢复原始终端属性。
async fn shutdown_signal(state: AppState) {
    let (quit_tx, mut quit_rx) = oneshot::channel::<()>();

    // 退出令牌：停机时停掉轮询任务并等它恢复终端；spawn_blocking 任务
    // 不退出的话 runtime 关闭会一直等它，进程挂住退不出来。
    let stop = CancellationToken::new();
    let stop_keyboard = stop.clone();
    let keyboard = tokio::task::spawn_blocking(move || {
        let _key_guard = QuitKeyGuard::enter();
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
                tracing::info!("received Ctrl+C, cancelling run and stopping server");
                cancel_all(&state).await;
            }
        }
        _ = &mut quit_rx => {
            tracing::info!("received quit key, cancelling run and stopping server");
            cancel_all(&state).await;
        }
    }
    // 断掉全部 WebSocket 长连接：graceful shutdown 会等所有在途连接结束，WebSocket
    // 由前端持续持有，不主动关闭的话 serve 永不返回、进程挂住。
    state.inner.shutdown.cancel();
    stop.cancel();
    let _ = keyboard.await;
}

/// 停机时取消全部 session 的进行中运行（队列保留，进程即将退出）。
async fn cancel_all(state: &AppState) {
    let sessions = state.inner.sessions.lock().await;
    for session in sessions.values() {
        let _ = session.cancel_run();
    }
}

/// 退出键：q，或 Ctrl+C（仅 Windows 的 crossterm raw mode 下 Ctrl+C 以
/// `Char('c') + CONTROL` 按键事件送达；Unix 上 ISIG 保持开启，Ctrl+C 走
/// SIGINT，由 `tokio::signal::ctrl_c` 处理，不经这里）。
fn is_quit_key(key: event::KeyEvent) -> bool {
    key.code == KeyCode::Char('q')
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
}

/// 退出键监听的终端态 RAII：q 按下即送达且不回显；离开作用域（含轮询
/// 任务 panic 经 Drop）恢复原始终端属性，把 tty 还给 shell。
///
/// Unix 上刻意不用 crossterm raw mode：raw mode 是 cfmakeraw 语义，会清掉
/// OPOST/ONLCR，web 服务存活期间打印到终端的 \n 不再自动带回车（输出
/// 阶梯错位），子进程继承 tty 也受影响。这里只关 ICANON（行缓冲）与
/// ECHO（回显），输出处理保持原样——只有 TUI 模式才关 ONLCR。Windows
/// 控制台无 ONLCR 概念，直接用 crossterm raw mode。
struct QuitKeyGuard {
    /// 进入前的终端属性；stdin 非 tty（管道等）时为 None，无需恢复。
    #[cfg(unix)]
    saved: Option<Termios>,
}

impl QuitKeyGuard {
    fn enter() -> Self {
        #[cfg(unix)]
        {
            Self {
                saved: enter_quit_key_mode().ok(),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = crossterm::terminal::enable_raw_mode();
            Self
        }
    }
}

impl Drop for QuitKeyGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(saved) = self.saved.take() {
            let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &saved);
        }
        #[cfg(not(unix))]
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Unix：仅关闭 ICANON/ECHO，让按键立即送达且不回显；VMIN=1/VTIME=0 保证
/// 单键即唤醒 read（防御终端原有非规范配置）。返回进入前的终端属性供恢复。
#[cfg(unix)]
fn enter_quit_key_mode() -> nix::Result<Termios> {
    let stdin = std::io::stdin();
    let saved = tcgetattr(&stdin)?;
    let mut attrs = saved.clone();
    attrs
        .local_flags
        .remove(LocalFlags::ICANON | LocalFlags::ECHO);
    attrs.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
    attrs.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
    tcsetattr(&stdin, SetArg::TCSANOW, &attrs)?;
    Ok(saved)
}

#[cfg(test)]
pub mod tests;
