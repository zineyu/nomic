//! 交互 TUI（ratatui + crossterm，设计见 docs/adr/0002）。
//!
//! 结构：
//! - [`app`]：纯状态层——对外为语义操作（按键 [`app::Key`] → [`app::Effect`]、
//!   应用事件、滚动、会话/附件管理），编辑器/补全/picker/slash 分发是其内部实现，
//!   脱离终端可测
//! - [`ui`]：纯渲染（聊天区 + 输入框 + 状态栏）
//! - 本文件：终端生命周期、事件循环（`KeyEvent` → `Key` 映射、`Effect` 接线执行）、
//!   agent driver 任务
//!
//! agent 由专属 tokio 任务持有（`Agent::prompt` 需要 `&mut self` 且跨轮复用），
//! TUI 经 mpsc 发送 prompt（附本轮 `CancellationToken`），agent 事件经既有
//! channel 回流；`MessageEnd` 定稿点复用事件驱动落库。
//!
//! 错误策略：可预期错误（agent loop 失败、压缩失败、落库失败等）就地转为
//! 状态栏/聊天区提示；意外错误（driver 任务 panic）经 JoinHandle 捕获后在
//! 聊天区提示，TUI 保持存活供查看记录，而非静默退出。

mod app;
mod markdown;
mod theme;
mod ui;

use std::io;

use anyhow::{Context as _, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use futures::StreamExt as _;
use nomic_ai::{Message, Model, ThinkingLevel};
use nomic_core::{Agent, AgentEvent, Compaction, estimate_context_tokens};
use nomic_session::{CompactionRecord, SessionStore, TreeEntry};
use nomic_skills::SkillResolver;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use app::{App, Effect, Key, PickerRow, SkillEntry};

use crate::bootstrap::{ModelChoice, ModelResolver};
use crate::{Cli, bootstrap};

/// 提交给 agent driver 的任务。
enum DriverJob {
    /// 运行一轮 prompt（附图片附件与本轮取消令牌）
    Prompt(String, Vec<nomic_ai::ImageContent>, CancellationToken),
    /// 手动压缩上下文（`/compact [聚焦指令]`，附本轮取消令牌）
    Compact(Option<String>, CancellationToken),
    /// 重试最近一轮失败的响应（`/retry`，附本轮取消令牌）
    Retry(CancellationToken),
    /// 向 agent 历史注入一条 user 消息（`/skill:<name>` 手动载入），不启动 run
    Inject(String),
    /// 清空 agent 上下文（`/new`）
    Clear,
    /// 整体替换 agent 上下文（`/resume` 恢复历史 session）
    Restore(Vec<Message>),
    /// 切换模型（`/models`；上下文保留，spec 已按启动同一口径解析）
    SwitchModel(Model),
    /// 设置思考级别（模型切换流程第二步确认；None 关闭）
    SetReasoning(Option<ThinkingLevel>),
}

/// agent driver 完成的任务回执。
enum DriverDone {
    /// 一轮 prompt 结束（Err 为 agent loop 错误）
    Prompt(Result<(), String>),
    /// 一次手动压缩结束（Ok(None) 表示无可压缩内容；Err 为摘要失败）
    Compact(Result<Option<Compaction>, String>),
    /// 一次重试结束（Ok(false) 表示无可重试状态；Err 为 loop 错误）
    Retry(Result<bool, String>),
    /// 上下文 token 估算回报（每个 job 处理后发送，状态栏用量显示用）
    Context(u64),
}

/// 运行交互 TUI。
pub async fn run(cli: &Cli) -> Result<()> {
    let boot = bootstrap::bootstrap(cli).await?;

    let mut app = App::new(
        boot.model.name.clone(),
        boot.session.as_ref().map(|(_, id)| id.clone()),
        boot.model.context_window,
    );
    app.load_history(&boot.history);
    // `--image` 附件在 TUI 模式同样生效：作为首轮消息的暂存附件
    stage_cli_images(&mut app, &cli.image);
    let skill_resolver = boot.skill_resolver.clone();
    app.set_available_skills(
        skill_resolver
            .catalog()
            .into_iter()
            .map(|skill| SkillEntry {
                name: skill.name,
                description: skill.document.description,
                scope: skill.scope,
            })
            .collect(),
    );
    app.set_available_templates(boot.prompt_templates.clone());
    // 启动解析的思考级别（CLI 参数 / 配置文件）在进入 builder 前取出，
    // driver 据此维护 `/models` 级别选择器的当前值
    let initial_reasoning = boot.stream_options.reasoning;

    let (agent, mut events) = Agent::builder()
        .model(boot.model.clone())
        .provider(boot.provider)
        .system_prompt(boot.system_prompt)
        .tools(nomic_tools::default_tools_with_skills(boot.skill_resolver))
        .messages(boot.history)
        .stream_options(boot.stream_options)
        .compaction(boot.compaction)
        .build();

    // 落库父指针：恢复的 session 从默认分支末端起算（分支场景下保证续写
    // 落在默认分支而非全局最新 entry）；读取失败退回自动链最新
    let mut tip = None;
    if let Some((store, id)) = &boot.session {
        match store.latest_entry_id(id).await {
            Ok(latest) => tip = latest,
            Err(error) => app.warn(format!("读取分支末端失败，落库将链到最新 entry：{error}")),
        }
    }

    let _guard = TerminalGuard::enter().context("初始化终端失败")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("创建终端后端失败")?;

    let (mut driver, mut done_rx) = spawn_driver(
        agent,
        boot.session,
        boot.models,
        boot.model,
        skill_resolver,
        tip,
        initial_reasoning,
    )?;
    let mut term_events = EventStream::new();
    // spinner 帧推进：仅运行中需要动画，空闲时分支挂起不唤醒事件循环
    let mut spinner_ticker = tokio::time::interval(std::time::Duration::from_millis(100));
    loop {
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .context("绘制失败")?;
        let wake = next_wake(
            app.is_running(),
            &mut driver,
            &mut term_events,
            &mut spinner_ticker,
            &mut events,
            &mut done_rx,
        )
        .await;
        if handle_wake(wake, &mut app, &mut driver).await || app.should_quit() {
            break;
        }
    }
    Ok(())
}

/// 启动 agent driver：专属 tokio 任务持有 Agent，串行执行 job，完成后回传结果。
fn spawn_driver(
    agent: Agent,
    session: Option<(SessionStore, String)>,
    models: ModelResolver,
    model: Model,
    skill_resolver: SkillResolver,
    tip: Option<String>,
    reasoning: Option<ThinkingLevel>,
) -> Result<(Driver, mpsc::UnboundedReceiver<DriverDone>)> {
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<DriverJob>();
    let (done_tx, done_rx) = mpsc::unbounded_channel::<DriverDone>();
    let driver_task = tokio::spawn(async move {
        let mut agent = agent;
        while let Some(job) = job_rx.recv().await {
            match job {
                DriverJob::Prompt(text, images, cancel) => {
                    let result = agent
                        .prompt_with_images(&text, &images, cancel)
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string());
                    if done_tx.send(DriverDone::Prompt(result)).is_err() {
                        return;
                    }
                }
                DriverJob::Compact(instructions, cancel) => {
                    let result = agent
                        .compact(instructions.as_deref(), cancel)
                        .await
                        .map_err(|error| error.to_string());
                    if done_tx.send(DriverDone::Compact(result)).is_err() {
                        return;
                    }
                }
                DriverJob::Retry(cancel) => {
                    let result = agent
                        .retry(cancel)
                        .await
                        .map(|outcome| outcome.is_some())
                        .map_err(|error| error.to_string());
                    if done_tx.send(DriverDone::Retry(result)).is_err() {
                        return;
                    }
                }
                DriverJob::Inject(text) => agent.inject_user_message(&text),
                DriverJob::Clear => agent.clear_messages(),
                DriverJob::Restore(messages) => agent.restore_messages(messages),
                DriverJob::SwitchModel(model) => agent.set_model(model),
                DriverJob::SetReasoning(level) => agent.set_reasoning(level),
            }
            // 每个 job 都可能改变上下文：回报最新 token 估算（与自动压缩同一口径）
            let tokens = estimate_context_tokens(agent.messages());
            if done_tx.send(DriverDone::Context(tokens)).is_err() {
                return;
            }
        }
    });
    let driver = Driver {
        job_tx,
        current_cancel: None,
        task: Some(driver_task),
        alive: true,
        session,
        tip,
        cwd: std::env::current_dir().context("get cwd")?,
        skill_resolver,
        models,
        model,
        reasoning,
        pending_model: None,
    };
    Ok((driver, done_rx))
}

/// 事件循环持有的驱动端资源。
struct Driver {
    job_tx: mpsc::UnboundedSender<DriverJob>,
    current_cancel: Option<CancellationToken>,
    /// driver 任务的 JoinHandle：任务 panic 时取出详情转为 TUI 内错误提示
    task: Option<tokio::task::JoinHandle<()>>,
    /// driver 是否存活；退出后其 channel 已关闭，事件循环跳过对应分支
    alive: bool,
    session: Option<(SessionStore, String)>,
    /// 落库父指针：当前分支末端的 entry id（新 session 或无条目时为 None，
    /// 追加时自动链到最新 entry）。`/tree` 创建分支即切换该指针；每次成功
    /// 落库后推进到新 entry。
    tip: Option<String>,
    cwd: std::path::PathBuf,
    skill_resolver: SkillResolver,
    /// 运行时模型解析器（`/models` 候选与切换，与启动同一分层口径）
    models: ModelResolver,
    /// 当前模型（`/models` 切换后更新；选择器预选与切换幂等判断用）
    model: Model,
    /// 当前思考级别（`/models` 级别选择器确认后更新；预选与幂等判断用）
    reasoning: Option<ThinkingLevel>,
    /// 待切换模型（模型切换流程第二步暂存）：模型选择器确认后、思考级别
    /// 选择器确认（应用切换）或 Esc（放弃切换）前持有
    pending_model: Option<Model>,
}

/// 事件循环单次等待的结果。
enum Wake {
    /// 按键（Press/Repeat）
    Key(KeyEvent),
    /// 鼠标滚轮
    ScrollUp,
    ScrollDown,
    /// bracketed paste（终端粘贴/拖入的整段文本）
    Paste(String),
    /// agent 事件
    AgentEvent(AgentEvent),
    /// driver 任务完成（prompt 或手动压缩）
    AgentDone(DriverDone),
    /// spinner 帧推进
    Tick,
    /// 仅需重绘（resize、其他鼠标事件）
    Redraw,
    /// agent driver 任务意外退出（panic 或提前返回），附详情
    DriverFailed(String),
    /// 终端事件流关闭：无法继续交互，退出循环
    TermClosed,
}

/// 处理一次唤醒；返回 `true` 表示终端事件流关闭、退出循环。
async fn handle_wake(wake: Wake, app: &mut App, driver: &mut Driver) -> bool {
    match wake {
        Wake::Key(key) => {
            // Ctrl+V 粘贴需异步读剪贴板，先于语义映射拦截
            if matches!(
                (key.code, key.modifiers),
                (KeyCode::Char('v'), KeyModifiers::CONTROL)
            ) {
                paste_clipboard(app).await;
            } else if let Some(key) = map_key(key) {
                for effect in app.press(key) {
                    execute_effect(app, driver, effect).await;
                }
            }
        }
        Wake::ScrollUp => app.scroll_up(3),
        Wake::ScrollDown => app.scroll_down(3),
        Wake::Paste(text) => handle_paste(app, &text),
        Wake::AgentEvent(event) => {
            match &event {
                AgentEvent::MessageEnd(message) => {
                    persist(driver, message, app).await;
                }
                AgentEvent::CompactionEnd {
                    summary,
                    tokens_before,
                    kept_count,
                    ..
                } => {
                    persist_compaction(driver, summary, *tokens_before, *kept_count, app).await;
                }
                _ => {}
            }
            app.handle_event(&event);
        }
        Wake::AgentDone(done) => match done {
            DriverDone::Prompt(result) => {
                driver.current_cancel = None;
                let notice = result
                    .err()
                    .map(|error| format!("agent loop 失败：{error}"));
                app.finish_run(notice);
            }
            DriverDone::Compact(result) => {
                driver.current_cancel = None;
                let notice = match result {
                    // 压缩成功经 CompactionEnd 事件渲染与落库，这里无需重复处理
                    Ok(Some(_)) => None,
                    Ok(None) => Some("上下文很短，没有可压缩的内容。".to_string()),
                    Err(error) => Some(format!("压缩失败，上下文保持不变：{error}")),
                };
                app.finish_run(notice);
            }
            DriverDone::Retry(result) => {
                driver.current_cancel = None;
                let notice = match result {
                    // 重试成功经事件流渲染与落库，这里无需重复处理
                    Ok(true) => None,
                    Ok(false) => Some("最近一轮没有失败的响应，无需重试。".to_string()),
                    Err(error) => Some(format!("重试失败：{error}")),
                };
                app.finish_run(notice);
            }
            DriverDone::Context(tokens) => app.set_context_tokens(tokens),
        },
        Wake::Tick => app.tick(),
        Wake::Redraw => {}
        Wake::DriverFailed(detail) => {
            tracing::error!(detail, "agent driver 任务意外退出");
            // 进行中的一轮永远不会回执：回到空闲态，避免 spinner 空转
            if app.is_running() {
                app.finish_run(None);
            }
            app.push_system(format!(
                "内部错误：agent 任务意外退出（{detail}）。对话记录仍可查看，但无法继续发送消息。"
            ));
        }
        Wake::TermClosed => return true,
    }
    false
}

/// 等待下一个唤醒源：按键 / 鼠标 / agent 事件 / 本轮完成 / spinner 帧。
///
/// agent 侧 channel 与 driver 任务同生命周期：channel 关闭即任务退出
/// （job 发送端不会先于任务丢弃），统一转为 [`Wake::DriverFailed`]。
async fn next_wake(
    running: bool,
    driver: &mut Driver,
    term_events: &mut EventStream,
    spinner_ticker: &mut tokio::time::Interval,
    events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    done_rx: &mut mpsc::UnboundedReceiver<DriverDone>,
) -> Wake {
    let driver_alive = driver.alive;
    tokio::select! {
        // spinner 动画仅在运行中推进；空闲时此分支永久挂起，不空转重绘
        () = async {
            if running {
                spinner_ticker.tick().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => Wake::Tick,
        maybe_event = term_events.next() => match maybe_event {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => Wake::Key(key),
            Some(Ok(Event::Paste(text))) => Wake::Paste(text),
            Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                MouseEventKind::ScrollUp => Wake::ScrollUp,
                MouseEventKind::ScrollDown => Wake::ScrollDown,
                _ => Wake::Redraw,
            },
            Some(Ok(_)) => Wake::Redraw,
            Some(Err(_)) | None => Wake::TermClosed,
        },
        // driver 退出后 channel 已关闭，分支挂起避免立即返回 None 空转
        maybe_event = async {
            if driver_alive {
                events.recv().await
            } else {
                std::future::pending().await
            }
        } => match maybe_event {
            Some(event) => Wake::AgentEvent(event),
            None => driver_failed(driver).await,
        },
        maybe_done = async {
            if driver_alive {
                done_rx.recv().await
            } else {
                std::future::pending().await
            }
        } => match maybe_done {
            Some(done) => Wake::AgentDone(done),
            None => driver_failed(driver).await,
        },
    }
}

/// driver 任务退出：取出 JoinHandle 详情（panic 负载等），转为 TUI 内提示。
async fn driver_failed(driver: &mut Driver) -> Wake {
    driver.alive = false;
    let detail = match &mut driver.task {
        Some(handle) => match handle.await {
            Ok(()) => "任务提前结束".to_string(),
            Err(error) if error.is_panic() => {
                let payload = error.into_panic();
                format!("panic：{}", panic_payload_text(&*payload))
            }
            Err(error) => error.to_string(),
        },
        // 已报告过一次（events 与 done 两个 channel 先后关闭）
        None => "任务已退出".to_string(),
    };
    driver.task = None;
    Wake::DriverFailed(detail)
}

/// 提取 panic 负载文本（`panic!("...")` 的 `&str`/`String`），无法识别时给兜底描述。
fn panic_payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_string()
    } else {
        "未知负载".to_string()
    }
}

/// 把 crossterm 按键映射为状态层的语义按键；未识别的组合返回 `None`。
const fn map_key(key: KeyEvent) -> Option<Key> {
    Some(match (key.code, key.modifiers) {
        (KeyCode::Char(c), KeyModifiers::CONTROL) => Key::Ctrl(c),
        (KeyCode::Esc, _) => Key::Esc,
        (KeyCode::Tab, _) => Key::Tab,
        // 换行必须在提交之前匹配；依赖 kitty 键盘增强协议区分两者
        (KeyCode::Enter, KeyModifiers::SHIFT) => Key::Newline,
        (KeyCode::Enter, _) => Key::Enter,
        (KeyCode::Backspace, _) => Key::Backspace,
        (KeyCode::Left, _) => Key::Left,
        (KeyCode::Right, _) => Key::Right,
        (KeyCode::Home, _) => Key::Home,
        (KeyCode::End, _) => Key::End,
        (KeyCode::Up, _) => Key::Up,
        (KeyCode::Down, _) => Key::Down,
        (KeyCode::PageUp, _) => Key::PageUp,
        (KeyCode::PageDown, _) => Key::PageDown,
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => Key::Char(c),
        _ => return None,
    })
}

/// 执行 [`App::press`] 返回的语义效果：driver job、取消令牌、session 库、
/// skill resolver、图片加载等外部资源在此接线。
async fn execute_effect(app: &mut App, driver: &mut Driver, effect: Effect) {
    match effect {
        Effect::Prompt { text, images } => {
            let token = CancellationToken::new();
            if driver
                .job_tx
                .send(DriverJob::Prompt(text, images, token.clone()))
                .is_ok()
            {
                driver.current_cancel = Some(token);
            } else {
                // driver 已退出：不会有回执，立即回到空闲态并提示
                app.finish_run(Some("内部错误：agent 任务已退出，消息未发送。".to_string()));
            }
        }
        Effect::Compact(instructions) => {
            let token = CancellationToken::new();
            if driver
                .job_tx
                .send(DriverJob::Compact(instructions, token.clone()))
                .is_ok()
            {
                driver.current_cancel = Some(token);
            } else {
                app.finish_run(Some("内部错误：agent 任务已退出，无法压缩。".to_string()));
            }
        }
        Effect::Retry => {
            let token = CancellationToken::new();
            if driver.job_tx.send(DriverJob::Retry(token.clone())).is_ok() {
                driver.current_cancel = Some(token);
            } else {
                app.finish_run(Some("内部错误：agent 任务已退出，无法重试。".to_string()));
            }
        }
        Effect::Cancel => {
            if let Some(token) = &driver.current_cancel {
                token.cancel();
            }
        }
        Effect::ListSessions => list_sessions(app, driver).await,
        Effect::Resume(id) => {
            resume_session(app, driver, id).await;
        }
        Effect::ListTree => list_tree(app, driver).await,
        Effect::BranchTo(entry_id) => branch_to(app, driver, entry_id).await,
        Effect::ListModels => list_models(app, driver),
        Effect::SwitchModel(id) => select_model(app, driver, &id),
        Effect::SetReasoning(level) => set_reasoning(app, driver, &level),
        Effect::CancelModelSwitch => {
            // 模型切换流程第二步被取消：放弃待切换模型，模型与级别均不变
            if driver.pending_model.take().is_some() {
                app.push_system("已取消模型切换。".to_string());
            }
        }
        Effect::ListSkills => {
            // 列出时顺带刷新补全快照：会话期间新增的 skill 也能被 Tab 补全
            let catalog = driver.skill_resolver.catalog();
            app.show_skills(
                catalog
                    .into_iter()
                    .map(|skill| SkillEntry {
                        name: skill.name,
                        description: skill.document.description,
                        scope: skill.scope,
                    })
                    .collect(),
            );
        }
        Effect::LoadSkill(name) => match driver.skill_resolver.activate(&name) {
            Ok(skill) => {
                // 注入消息经事件管线回流：聊天区压缩展示 + session 落库自动生效
                let _ = driver
                    .job_tx
                    .send(DriverJob::Inject(app::skill_load_message(&skill)));
            }
            Err(error) => app.warn(format!("载入 skill {name:?} 失败：{error}")),
        },
        Effect::AttachImage(path) => attach_image(app, &std::path::PathBuf::from(path)),
        Effect::CopyText(text) => copy_to_clipboard(app, text).await,
        Effect::NewSession => new_session(app, driver).await,
    }
}

/// `/models`：列出当前 provider 的候选模型并打开选择器（预选中当前模型）。
fn list_models(app: &mut App, driver: &Driver) {
    let current = &driver.model.id;
    let choices = driver.models.candidates(&driver.model.provider, current);
    if choices.is_empty() {
        // 理论不可达（候选至少含当前模型），防御 provider 配置在运行期失效
        app.warn("没有可用的模型候选");
        return;
    }
    let selected = choices
        .iter()
        .position(|choice| &choice.id == current)
        .unwrap_or(0);
    let rows = choices
        .iter()
        .map(|choice| PickerRow {
            id: choice.id.clone(),
            text: model_row_text(choice, current),
            selectable: true,
        })
        .collect();
    app.open_model_picker(rows, selected);
}

/// 选择器行文本：`id — 展示名 · ctx 200k · 支持思考`，当前模型带标记；
/// 窗口未知省略 ctx。
fn model_row_text(choice: &ModelChoice, current_id: &str) -> String {
    use std::fmt::Write as _;
    let mut text = format!("{} — {}", choice.id, choice.name);
    if choice.context_window > 0 {
        let _ = write!(text, " · ctx {}", ui::format_tokens(choice.context_window));
    }
    if choice.reasoning {
        text.push_str(" · 支持思考");
    }
    if choice.id == current_id {
        text.push_str("（当前）");
    }
    text
}

/// 思考级别选择器确认时的解析结果：关闭（`off`）或具体级别。
///
/// 独立于 `Option<ThinkingLevel>`：让「行 id 非法」（None，拒绝）与
/// 「off 关闭」（合法设置）在类型层面可区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningSetting {
    /// 关闭思考
    Off,
    /// 具体思考级别
    Level(ThinkingLevel),
}

impl ReasoningSetting {
    /// 转为请求参数（`Off` → `None` 关闭）。
    const fn level(self) -> Option<ThinkingLevel> {
        match self {
            Self::Off => None,
            Self::Level(level) => Some(level),
        }
    }
}

/// 思考级别词表：选择器行 id 与展示说明共用同一来源。
const REASONING_LEVELS: [(&str, ReasoningSetting); 5] = [
    ("off", ReasoningSetting::Off),
    ("minimal", ReasoningSetting::Level(ThinkingLevel::Minimal)),
    ("low", ReasoningSetting::Level(ThinkingLevel::Low)),
    ("medium", ReasoningSetting::Level(ThinkingLevel::Medium)),
    ("high", ReasoningSetting::Level(ThinkingLevel::High)),
];

/// 级别词 → 设置；未知词返回 `None`（调用方告警）。
fn reasoning_setting(word: &str) -> Option<ReasoningSetting> {
    REASONING_LEVELS
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(_, setting)| *setting)
}

/// 当前级别 → 词表中的级别词（提示文本用；词表外取值回退 `off`）。
fn reasoning_label(level: Option<ThinkingLevel>) -> &'static str {
    REASONING_LEVELS
        .iter()
        .find(|(_, setting)| setting.level() == level)
        .map_or("off", |(name, _)| *name)
}

/// 思考级别选择器（模型切换流程第二步）：列出级别并打开选择器
///（预选中当前级别）。
fn open_reasoning_picker(app: &mut App, driver: &Driver) {
    let current = driver.reasoning;
    let rows = REASONING_LEVELS
        .iter()
        .map(|(name, setting)| PickerRow {
            id: (*name).to_string(),
            text: reasoning_row_text(name, *setting, current),
            selectable: true,
        })
        .collect();
    let selected = REASONING_LEVELS
        .iter()
        .position(|(_, setting)| setting.level() == current)
        .unwrap_or(0);
    app.open_reasoning_picker(rows, selected);
}

/// 思考级别选择器行文本：`级别 — 说明`，当前级别带标记。
fn reasoning_row_text(
    name: &str,
    setting: ReasoningSetting,
    current: Option<ThinkingLevel>,
) -> String {
    let description = match setting {
        ReasoningSetting::Off => "不开启思考",
        ReasoningSetting::Level(ThinkingLevel::Minimal) => "最小推理预算",
        ReasoningSetting::Level(ThinkingLevel::Low) => "低推理预算",
        ReasoningSetting::Level(ThinkingLevel::Medium) => "中等推理预算",
        ReasoningSetting::Level(ThinkingLevel::High) => "高推理预算",
        // xhigh/max 不在 TUI 词表内（配置文件与 CLI 同样不开放）
        ReasoningSetting::Level(ThinkingLevel::Xhigh | ThinkingLevel::Max) => "推理预算",
    };
    let mut text = format!("{name} — {description}");
    if setting.level() == current {
        text.push_str("（当前）");
    }
    text
}

/// 思考级别选择器确认（模型切换流程第二步）：先应用待切换模型，
/// 再设置思考级别；两者均未变化时提示。
///
/// 级别是请求参数，选择器只在目标模型支持推理时出现，因此设置必然
/// 随请求生效（重选当前模型进入时当前模型即推理模型）。driver 串行
/// 处理任务：级别设置一定排在模型切换之后。
fn set_reasoning(app: &mut App, driver: &mut Driver, word: &str) {
    use std::fmt::Write as _;
    let Some(setting) = reasoning_setting(word) else {
        // 理论不可达（选择器行 id 出自 REASONING_LEVELS 词表）
        app.warn(format!("未知思考级别 {word:?}"));
        return;
    };
    let level = setting.level();
    let switched = match driver.pending_model.take() {
        Some(model) => apply_model_switch(app, driver, model),
        None => false,
    };
    let level_changed = level != driver.reasoning;
    if level_changed {
        if driver.job_tx.send(DriverJob::SetReasoning(level)).is_err() {
            app.warn("内部错误：agent 任务已退出，无法设置思考级别");
            return;
        }
        driver.reasoning = level;
    }
    let mut parts: Vec<String> = Vec::new();
    if switched {
        let mut part = format!("已切换模型为 {}", driver.model.name);
        if driver.model.context_window > 0 {
            let _ = write!(
                part,
                "（ctx {}）",
                ui::format_tokens(driver.model.context_window)
            );
        }
        parts.push(part);
    }
    if level_changed {
        parts.push(format!("思考级别设为 {}", reasoning_label(level)));
    }
    let text = if parts.is_empty() {
        format!(
            "模型与思考级别均未变化（{}，级别 {}）。",
            driver.model.name,
            reasoning_label(driver.reasoning)
        )
    } else if switched {
        format!("{}，对话上下文保留。", parts.join("，"))
    } else {
        format!("{}。", parts.join("，"))
    };
    app.push_system(text);
}

/// `/models:<id>` 或模型选择器确认：先选模型后选 effort——
///
/// - 目标模型支持推理：暂存为待切换模型并打开思考级别选择器（流程第二步）；
///   确认级别时一并应用切换，Esc 放弃整个切换。重选当前模型时不暂存，
///   级别选择器变为单纯的级别调整入口
/// - 目标模型不支持推理：直接切换（级别设置保留但随请求被忽略，
///   与配置文件 `reasoning` 同一口径）
fn select_model(app: &mut App, driver: &mut Driver, id: &str) {
    match driver.models.resolve(&driver.model.provider, id) {
        Err(error) => app.warn(format!("切换模型失败：{error:#}")),
        Ok(model) if model.reasoning => {
            driver.pending_model = (model.id != driver.model.id).then_some(model);
            open_reasoning_picker(app, driver);
        }
        Ok(model) if model.id == driver.model.id => {
            app.push_system(format!("当前模型已是 {}（不支持思考）。", model.name));
        }
        Ok(model) => {
            if apply_model_switch(app, driver, model) {
                app.push_system(switch_notice(&driver.model));
            }
        }
    }
}

/// 发送 SwitchModel job 并同步 driver/app 状态（状态栏徽标、上下文窗口）；
/// 成功返回 `true`，driver 已退出时告警并返回 `false`。
///
/// driver 串行处理任务，紧随的级别设置与 prompt 一定跑在新模型上。
fn apply_model_switch(app: &mut App, driver: &mut Driver, model: Model) -> bool {
    if driver
        .job_tx
        .send(DriverJob::SwitchModel(model.clone()))
        .is_err()
    {
        app.warn("内部错误：agent 任务已退出，无法切换模型");
        return false;
    }
    let name = model.name.clone();
    let window = model.context_window;
    driver.model = model;
    app.set_model(name, window);
    true
}

/// 切换成功的提示文本：`已切换模型为 X（ctx 400k），对话上下文保留。`
fn switch_notice(model: &Model) -> String {
    use std::fmt::Write as _;
    let mut text = format!("已切换模型为 {}", model.name);
    if model.context_window > 0 {
        let _ = write!(text, "（ctx {}）", ui::format_tokens(model.context_window));
    }
    text.push_str("，对话上下文保留。");
    text
}
/// `/resume`：列出历史 session 并打开选择器。
async fn list_sessions(app: &mut App, driver: &Driver) {
    match session_store(driver.session.as_ref()).await {
        Err(error) => app.warn(format!("{error:#}")),
        Ok(store) => match store.list_sessions().await {
            Err(error) => app.warn(format!("列出 session 失败：{error}")),
            Ok(sessions) if sessions.is_empty() => {
                app.push_system("没有历史 session。");
            }
            Ok(sessions) => {
                let rows = sessions
                    .iter()
                    .map(|summary| PickerRow {
                        id: summary.id.clone(),
                        text: crate::sessions::row_text(summary),
                        selectable: true,
                    })
                    .collect();
                app.open_resume_picker(rows);
            }
        },
    }
}

/// `/new`：driver 串行清空上下文；本地重置聊天区并新建 session。
async fn new_session(app: &mut App, driver: &mut Driver) {
    // driver 串行处理任务；slash 命令仅在空闲时可提交，无需排队等待
    let _ = driver.job_tx.send(DriverJob::Clear);
    app.start_new_conversation();
    // 新 session 没有任何 entry：落库父指针重置（自动链最新）
    driver.tip = None;
    if let Some((store, id)) = &mut driver.session {
        match store.create_session(&driver.cwd).await {
            Ok(new_id) => {
                id.clone_from(&new_id);
                app.set_session(new_id);
            }
            Err(error) => {
                app.warn(format!("创建新 session 失败，续写当前 session：{error}"));
            }
        }
    }
}

/// `/tree`：列出当前 session 的会话树并打开选择器（预选中当前分支末端）。
async fn list_tree(app: &mut App, driver: &Driver) {
    let Some((store, session_id)) = &driver.session else {
        app.warn("当前对话未持久化，没有会话树可浏览");
        return;
    };
    match store.list_tree(session_id).await {
        Err(error) => app.warn(format!("加载会话树失败：{error}")),
        Ok(entries) if entries.is_empty() => {
            app.push_system("当前 session 还没有消息，发送一条后再来浏览会话树。");
        }
        Ok(entries) => {
            let rows = tree_rows(&entries, driver.tip.as_deref());
            // 预选中当前分支末端；末端不可选（如工具结果）时退到首个可选行
            let selected = driver
                .tip
                .as_deref()
                .and_then(|tip| entries.iter().position(|entry| entry.id == tip))
                .filter(|&index| entries[index].is_branchable())
                .or_else(|| entries.iter().position(TreeEntry::is_branchable))
                .expect("空树已在上面挡掉");
            app.open_tree_picker(rows, selected);
        }
    }
}

/// `/tree` 选择器确认：以所选条目为起点创建分支——重放该分支上下文、
/// 切换落库父指针；原分支 entries 不动，仍可在 `/tree` 中回访。
async fn branch_to(app: &mut App, driver: &mut Driver, entry_id: String) {
    let Some((store, session_id)) = &driver.session else {
        return; // ListTree 已挡住未持久化场景
    };
    if driver.tip.as_deref() == Some(entry_id.as_str()) {
        app.push_system("所选条目就是当前分支末端，无需切换。");
        return;
    }
    match store.load_branch(session_id, &entry_id).await {
        Err(error) => app.warn(format!("切换分支失败：{error}")),
        Ok(messages) => {
            // driver 串行处理任务：紧随其后的 prompt 一定排在 Restore 之后
            if driver
                .job_tx
                .send(DriverJob::Restore(messages.clone()))
                .is_err()
            {
                app.warn("内部错误：agent 任务已退出，无法切换分支");
                return;
            }
            let count = messages.len();
            app.restore_branch(&messages);
            driver.tip = Some(entry_id);
            app.push_system(format!(
                "已从所选条目创建分支（{count} 条消息），后续对话写入新分支；\
                 原分支保留，仍可在 /tree 中回访。"
            ));
        }
    }
}

/// 会话树条目 → 选择器行：按祖先链深度缩进，工具调用条目不可选，
/// 当前分支末端带标记。
fn tree_rows(entries: &[TreeEntry], tip: Option<&str>) -> Vec<PickerRow> {
    let index: std::collections::HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.id.as_str(), i))
        .collect();
    let depth = |entry: &TreeEntry| {
        let mut depth = 0_usize;
        let mut cursor = entry.parent_id.as_deref();
        // 父指针在写入时校验存在，父链必然终止于根；缺失数据按根处理
        while let Some(parent) = cursor {
            depth += 1;
            cursor = index
                .get(parent)
                .and_then(|&i| entries[i].parent_id.as_deref());
        }
        depth
    };
    entries
        .iter()
        .map(|entry| {
            let role = match entry.role.as_str() {
                "user" => "用户",
                "assistant" => "助手",
                "tool_result" => "工具",
                _ => "压缩",
            };
            let current = if Some(entry.id.as_str()) == tip {
                "（当前）"
            } else {
                ""
            };
            PickerRow {
                id: entry.id.clone(),
                text: format!(
                    "{}{role} · {} · {}{current}",
                    "  ".repeat(depth(entry)),
                    crate::sessions::format_time(Some(entry.timestamp)),
                    entry.preview,
                ),
                selectable: entry.is_branchable(),
            }
        })
        .collect()
}

/// 加载图片并暂存为附件（`/image` 与粘贴图片路径共用）。
fn attach_image(app: &mut App, path: &std::path::Path) {
    match crate::images::load_image(path) {
        Ok(image) => {
            let name = attachment_name(path);
            let count = app.stage_image(name.clone(), image);
            app.push_system(format!(
                "已附加图片 {name}（共 {count} 张，随下一条消息发送）。"
            ));
        }
        Err(error) => app.warn(format!("附加图片失败：{error:#}")),
    }
}

/// 粘贴整段文本（bracketed paste）：形似图片路径的转为附件，其余原样插入输入框。
///
/// 「形似」只按扩展名初判（裸路径 / `file://` URI / 引号包裹均可），
/// 能否加载由 [`crate::images::load_image`] 复核；多行或普通文本走插入。
fn handle_paste(app: &mut App, text: &str) {
    if let Some(path) = paste_image_path(text) {
        attach_image(app, &path);
    } else {
        app.paste_text(text);
    }
}

/// 从粘贴文本中识别图片路径：单行、支持 file:// URI（含百分号解码）与引号包裹。
fn paste_image_path(text: &str) -> Option<std::path::PathBuf> {
    let text = text.trim();
    if text.is_empty() || text.contains(['\n', '\t']) {
        return None;
    }
    let candidate = if let Some(uri) = text.strip_prefix("file://") {
        // file:///abs/path 与 file://localhost/abs/path
        let uri = uri.strip_prefix("localhost").unwrap_or(uri);
        percent_decode(uri)?
    } else {
        text.trim_matches(['\'', '"']).to_string()
    };
    let path = std::path::PathBuf::from(candidate);
    if crate::images::is_supported_image_path(&path) {
        Some(path)
    } else {
        None
    }
}

/// 百分号解码（file:// URI 中的 %20 等）；非法序列或结果非 UTF-8 返回 None。
fn percent_decode(input: &str) -> Option<String> {
    if !input.contains('%') {
        return Some(input.to_string());
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = input.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Ctrl+V 粘贴剪贴板：图片暂存为附件，文本插入输入框。
///
/// 剪贴板读取可能阻塞在 X11/Wayland 往返上，放 `spawn_blocking` 中执行；
/// 期间事件循环不阻塞，结果返回前界面照常重绘。
async fn paste_clipboard(app: &mut App) {
    match tokio::task::spawn_blocking(crate::clipboard::read).await {
        Ok(Ok(Some(crate::clipboard::ClipboardContent::Image(image)))) => {
            let name = format!("clipboard-{}.png", nomic_ai::now_millis());
            let count = app.stage_image(name.clone(), image);
            app.push_system(format!(
                "已粘贴图片 {name}（共 {count} 张，随下一条消息发送）。"
            ));
        }
        Ok(Ok(Some(crate::clipboard::ClipboardContent::Text(text)))) => app.paste_text(&text),
        Ok(Ok(None)) => app.warn("剪贴板中没有图片或文本"),
        Ok(Err(error)) => app.warn(format!("粘贴失败：{error:#}")),
        Err(join) => app.warn(format!("粘贴失败：{join}")),
    }
}

/// `/copy`：把文本写入系统剪贴板。
///
/// 与粘贴同理，写入可能阻塞在 X11/Wayland 往返上，放 `spawn_blocking` 中执行。
async fn copy_to_clipboard(app: &mut App, text: String) {
    let chars = text.chars().count();
    match tokio::task::spawn_blocking(move || crate::clipboard::write_text(&text)).await {
        Ok(Ok(())) => app.push_system(format!("已复制最新一条消息到剪贴板（{chars} 字）。")),
        Ok(Err(error)) => app.warn(format!("复制失败：{error:#}")),
        Err(join) => app.warn(format!("复制失败：{join}")),
    }
}

/// 附件展示名：取文件名，缺失时回退完整路径。
fn attachment_name(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// 把启动参数 `--image` 载入为暂存附件（失败以系统条目提示，不中止启动）。
fn stage_cli_images(app: &mut App, paths: &[std::path::PathBuf]) {
    for path in paths {
        match crate::images::load_image(path) {
            Ok(image) => {
                let name = attachment_name(path);
                let count = app.stage_image(name.clone(), image);
                app.push_system(format!(
                    "已附加图片 {name}（共 {count} 张，随下一条消息发送）。"
                ));
            }
            Err(error) => app.push_system(format!("加载图片附件失败：{error:#}")),
        }
    }
}

/// 取可用 session store：优先复用当前 session 的；未持久化（启动时打开失败）
/// 时按需重开——`/resume` 成功后该 store 会随恢复的 session 一同被采用。
async fn session_store(session: Option<&(SessionStore, String)>) -> Result<SessionStore> {
    match session {
        Some((store, _)) => Ok(store.clone()),
        None => SessionStore::open_default()
            .await
            .context("打开 session 库失败"),
    }
}

/// 恢复选中 session：加载历史 → 替换 agent 上下文与聊天区 → 切换落库目标
/// 与落库父指针（默认分支末端）。
async fn resume_session(app: &mut App, driver: &mut Driver, id: String) {
    let loaded = async {
        let store = session_store(driver.session.as_ref()).await?;
        let messages = store
            .load_messages(&id)
            .await
            .with_context(|| "加载 session 历史失败".to_string())?;
        let tip = store
            .latest_entry_id(&id)
            .await
            .context("读取分支末端失败")?;
        Ok::<_, anyhow::Error>((store, messages, tip))
    }
    .await;
    match loaded {
        Err(error) => app.warn(format!("恢复 session 失败：{error:#}")),
        Ok((store, messages, tip)) => {
            // driver 串行处理任务：紧随其后的 prompt 一定排在 Restore 之后，
            // 不会出现「新 prompt 跑在旧上下文」的交错
            let _ = driver.job_tx.send(DriverJob::Restore(messages.clone()));
            app.restore_conversation(&messages, id.clone());
            driver.tip = tip;
            match &mut driver.session {
                Some((_, current)) => current.clone_from(&id),
                None => driver.session = Some((store, id.clone())),
            }
            let label = nomic_session::session_title(&messages)
                .map_or_else(String::new, |title| format!("「{title}」"));
            app.push_system(format!(
                "已恢复 session {label}（{} 条消息），后续对话续写该 session。",
                messages.len()
            ));
        }
    }
}

/// `MessageEnd` 定稿点落库：以当前分支末端为父 entry，成功后推进父指针；
/// 失败仅提示不中断（store 非权威源）。
async fn persist(driver: &mut Driver, message: &Message, app: &mut App) {
    let Some((store, session_id)) = &driver.session else {
        return;
    };
    match store
        .append_message(session_id, driver.tip.as_deref(), message)
        .await
    {
        Ok(entry_id) => driver.tip = Some(entry_id),
        Err(error) => app.warn(format!("session 落库失败：{error}")),
    }
}

/// `CompactionEnd` 落库压缩条目（父指针语义与 [`persist`] 一致）。
async fn persist_compaction(
    driver: &mut Driver,
    summary: &str,
    tokens_before: u64,
    kept_count: usize,
    app: &mut App,
) {
    let Some((store, session_id)) = &driver.session else {
        return;
    };
    let record = CompactionRecord {
        summary: summary.to_string(),
        kept_count: kept_count as u64,
        tokens_before,
    };
    match store
        .append_compaction(session_id, driver.tip.as_deref(), &record)
        .await
    {
        Ok(entry_id) => driver.tip = Some(entry_id),
        Err(error) => app.warn(format!("compaction 落库失败：{error}")),
    }
}

/// 终端状态守卫：进入 raw mode + alternate screen + 鼠标捕获；
/// Drop（含 panic 路径经 hook）时恢复。
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        // bracketed paste：终端粘贴/拖入的内容整体作为 Event::Paste 上报，
        // 便于识别图片路径；不支持的终端忽略该序列，退化为逐键事件
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        // 启用 kitty 键盘增强协议，让支持它的终端把 Ctrl+Enter 与 Enter
        // 区分开上报；不支持的终端忽略该序列，Ctrl+Enter 退化为提交
        if matches!(supports_keyboard_enhancement(), Ok(true)) {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        install_panic_hook();
        Ok(Self)
    }

    fn restore() {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste,
            PopKeyboardEnhancementFlags
        );
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::restore();
    }
}

/// panic 时先恢复终端再交给默认 hook，避免终端残留 raw mode。
///
/// 仅主线程（事件循环/渲染）的 panic 不可恢复，走上述路径；tokio 任务线程
/// 的 panic（agent driver、剪贴板读取等）会经 JoinError 回流为 TUI 内提示，
/// 此处只落日志——既不恢复终端（TUI 仍在运行），也不打印到 stderr（避免花屏）。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            TerminalGuard::restore();
            default_hook(info);
        } else {
            tracing::error!(%info, "任务线程 panic");
        }
    }));
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::path::PathBuf;

    use super::{
        ModelChoice, ReasoningSetting, model_row_text, panic_payload_text, paste_image_path,
        percent_decode, reasoning_label, reasoning_row_text, reasoning_setting, tree_rows,
    };
    use nomic_ai::ThinkingLevel;
    use nomic_session::TreeEntry;

    /// 会话树选择器行：按深度缩进、角色/时间/预览拼接，工具调用条目不可选，
    /// 当前分支末端带标记。
    #[test]
    fn tree_rows_indent_by_depth_and_mark_tip() {
        let entry = |id: &str, parent: Option<&str>, role: &str, tool_calls: bool| TreeEntry {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            role: role.to_string(),
            timestamp: 1_785_000_000_000,
            preview: format!("preview of {id}"),
            has_tool_calls: tool_calls,
        };
        let entries = vec![
            entry("root", None, "user", false),
            entry("a1", Some("root"), "assistant", true),
            entry("t1", Some("a1"), "tool_result", false),
            entry("b1", Some("root"), "user", false),
        ];

        let rows = tree_rows(&entries, Some("b1"));
        assert_eq!(rows.len(), 4);
        assert!(rows[0].text.starts_with("用户 · "), "{}", rows[0].text);
        assert!(rows[0].text.contains("preview of root"));
        assert!(rows[1].text.starts_with("  助手 · "), "{}", rows[1].text);
        assert!(rows[2].text.starts_with("    工具 · "), "{}", rows[2].text);
        assert!(rows[3].text.ends_with("（当前）"), "{}", rows[3].text);

        assert!(rows[0].selectable);
        assert!(!rows[1].selectable, "含工具调用的 assistant 条目不可选");
        assert!(!rows[2].selectable, "工具结果条目不可选");
        assert!(rows[3].selectable);
    }

    /// `/models` 选择器行：id + 展示名 + 窗口，推理模型带标注，当前模型带标记，
    /// 窗口未知省略 ctx。
    #[test]
    fn model_row_text_formats_window_and_marks_current() {
        let choice = ModelChoice {
            id: "gpt-5.2".to_string(),
            name: "GPT-5.2".to_string(),
            context_window: 400_000,
            reasoning: true,
        };
        assert_eq!(
            model_row_text(&choice, "gpt-5.2"),
            "gpt-5.2 — GPT-5.2 · ctx 400k · 支持思考（当前）"
        );
        assert_eq!(
            model_row_text(&choice, "other"),
            "gpt-5.2 — GPT-5.2 · ctx 400k · 支持思考"
        );
        let no_thinking = ModelChoice {
            reasoning: false,
            ..choice
        };
        assert_eq!(
            model_row_text(&no_thinking, "other"),
            "gpt-5.2 — GPT-5.2 · ctx 400k"
        );
        let unknown = ModelChoice {
            id: "m".to_string(),
            name: "m".to_string(),
            context_window: 0,
            reasoning: false,
        };
        assert_eq!(model_row_text(&unknown, "other"), "m — m");
    }

    /// 思考级别词表：off 映射为关闭，词表内级别往返一致，未知词拒绝。
    #[test]
    fn reasoning_setting_roundtrip_and_rejects_unknown() {
        assert_eq!(reasoning_setting("off"), Some(ReasoningSetting::Off));
        assert_eq!(
            reasoning_setting("minimal"),
            Some(ReasoningSetting::Level(ThinkingLevel::Minimal))
        );
        assert_eq!(
            reasoning_setting("high"),
            Some(ReasoningSetting::Level(ThinkingLevel::High))
        );
        assert_eq!(
            reasoning_setting("off").map(ReasoningSetting::level),
            Some(None)
        );
        assert_eq!(reasoning_setting("extreme"), None);
        // 词表内取值与 label 往返一致；词表外取值（xhigh/max）回退 off
        for (name, level) in [
            ("off", None),
            ("low", Some(ThinkingLevel::Low)),
            ("medium", Some(ThinkingLevel::Medium)),
            ("high", Some(ThinkingLevel::High)),
        ] {
            assert_eq!(reasoning_label(level), name);
        }
        assert_eq!(reasoning_label(Some(ThinkingLevel::Xhigh)), "off");
    }

    /// 思考级别选择器行：级别 + 说明，当前级别带标记。
    #[test]
    fn reasoning_row_text_marks_current() {
        assert_eq!(
            reasoning_row_text(
                "low",
                ReasoningSetting::Level(ThinkingLevel::Low),
                Some(ThinkingLevel::Low)
            ),
            "low — 低推理预算（当前）"
        );
        assert_eq!(
            reasoning_row_text("off", ReasoningSetting::Off, Some(ThinkingLevel::Low)),
            "off — 不开启思考"
        );
    }

    #[test]
    fn panic_payload_extracts_message() {
        let payload: Box<dyn Any + Send> = Box::new("boom");
        assert_eq!(panic_payload_text(&*payload), "boom");

        let payload: Box<dyn Any + Send> = Box::new("owned boom".to_string());
        assert_eq!(panic_payload_text(&*payload), "owned boom");

        let payload: Box<dyn Any + Send> = Box::new(42_i32);
        assert_eq!(panic_payload_text(&*payload), "未知负载");
    }

    #[test]
    fn paste_recognizes_plain_image_path() {
        assert_eq!(
            paste_image_path("/tmp/pic.png"),
            Some(PathBuf::from("/tmp/pic.png"))
        );
        // 相对路径与大写扩展名
        assert_eq!(
            paste_image_path("shots/UPPER.PNG"),
            Some(PathBuf::from("shots/UPPER.PNG"))
        );
    }

    #[test]
    fn paste_recognizes_file_uri_and_decodes() {
        assert_eq!(
            paste_image_path("file:///tmp/my%20pics/a%20b.png"),
            Some(PathBuf::from("/tmp/my pics/a b.png"))
        );
        assert_eq!(
            paste_image_path("file://localhost/tmp/pic.webp"),
            Some(PathBuf::from("/tmp/pic.webp"))
        );
    }

    #[test]
    fn paste_recognizes_quoted_path() {
        assert_eq!(
            paste_image_path("'/tmp/with space/pic.jpg'"),
            Some(PathBuf::from("/tmp/with space/pic.jpg"))
        );
        assert_eq!(
            paste_image_path("\"/tmp/pic.gif\""),
            Some(PathBuf::from("/tmp/pic.gif"))
        );
    }

    #[test]
    fn paste_ignores_non_image_text() {
        assert_eq!(paste_image_path("hello world"), None);
        assert_eq!(paste_image_path("/tmp/notes.txt"), None);
        assert_eq!(paste_image_path("multi\nline /tmp/pic.png"), None);
        assert_eq!(paste_image_path(""), None);
        // 非法百分号序列不视为路径
        assert_eq!(paste_image_path("file:///tmp/%zz.png"), None);
    }

    #[test]
    fn percent_decode_roundtrip() {
        assert_eq!(
            percent_decode("/a%20b/%E4%B8%AD.png"),
            Some("/a b/中.png".to_string())
        );
        assert_eq!(percent_decode("no-escape"), Some("no-escape".to_string()));
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%xy"), None);
    }
}
