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
    cursor::SetCursorStyle,
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
use nomic_ai::{Message, Model, StopReason, ThinkingLevel};
use nomic_core::{Agent, AgentEvent, Compaction, estimate_context_tokens};
use nomic_session::{CompactionRecord, SessionStore, TreeEntry};
use nomic_skills::SkillResolver;
use nomic_tools::TodoStore;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use app::{App, Effect, Key, Mode, PickerRow, SkillEntry};

use crate::bootstrap::{ModelChoice, ModelResolver, ModelSelection};
use crate::{Cli, bootstrap};

/// 事件循环持有的终端类型（draw 与外部编辑器后的全量重绘共用）。
type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

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
    /// 切换模型（`/models`；上下文保留，spec 已按启动同一口径解析；
    /// 跨 provider 时携带新连接实现）
    SwitchModel(ModelSwitch),
    /// 设置思考级别（模型切换流程第二步确认；None 关闭）
    SetReasoning(Option<ThinkingLevel>),
}

/// `/models` 模型切换载荷：跨 provider 时携带新连接实现与分层的 api_key。
struct ModelSwitch {
    model: Model,
    provider: Option<ProviderSwitch>,
}

/// 跨 provider 切换的新连接：provider 实现 + 按启动同一口径分层
/// （环境变量 > `providers.<名字>.api_key` > 平铺配置；CLI 的 `--api-key`
/// 属于启动 provider，不参与运行时切换分层）的 api_key。
struct ProviderSwitch {
    provider: std::sync::Arc<dyn nomic_ai::Provider>,
    api_key: Option<String>,
}

/// agent driver 完成的任务回执。
enum DriverDone {
    /// 一轮 prompt 结束（Err 为 agent loop 错误）
    Prompt(Result<PromptEnd, String>),
    /// 一次手动压缩结束（Ok(None) 表示无可压缩内容；Err 为摘要失败）
    Compact(Result<Option<Compaction>, String>),
    /// 一次重试结束（Ok(false) 表示无可重试状态；Err 为 loop 错误）
    Retry(Result<bool, String>),
    /// 上下文 token 估算回报（每个 job 处理后发送，状态栏用量显示用）
    Context(u64),
}

/// 一轮 prompt 的结束回执（goal 模式是否自动追问的判定依据）。
struct PromptEnd {
    /// 是否正常结束：用户取消（Ctrl+C）或响应以 Error/Aborted
    /// 收尾时为 false——失败与中断的恢复由用户主导，不自动追问
    ended_normally: bool,
}

/// goal 模式连续自动追问的次数上限：防止模型反复不收尾时失控循环
///（达到上限后暂停追问，用户手动继续或 `/goal` 重开后重新计数）。
const MAX_GOAL_NUDGES: u32 = 3;

/// goal 模式的追问提示词：列出未完成的 todo（pending / in_progress），
/// 要求模型继续完成；清单为空或没有未完成项时返回 `None`（不追问）。
///
/// 该文本作为 user 消息进入对话历史（聊天区可见、随 session 落库）。
fn goal_reminder_prompt(todos: &TodoStore) -> Option<String> {
    let incomplete = todos.incomplete();
    if incomplete.is_empty() {
        return None;
    }
    Some(format!(
        "[goal 模式] react loop 已停止，但 todo 清单还有未完成的任务：\n{}\n\
         请继续完成上述剩余任务：逐项推进，完成后立即用 todo_write 更新状态；全部完成前不要停止。",
        nomic_tools::render_todos(&incomplete)
    ))
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

    let todo_store = TodoStore::new();
    let (agent, mut events) = Agent::builder()
        .model(boot.model.clone())
        .provider(boot.provider)
        .system_prompt(boot.system_prompt)
        .tools(nomic_tools::default_tools_with_skills(
            boot.skill_resolver,
            todo_store.clone(),
        ))
        .messages(boot.history)
        .stream_options(boot.stream_options)
        .compaction(boot.compaction)
        // 统一消息队列（ADR-0014）：TUI 与 agent 共享同一份，运行中
        // Enter 直推入队，core 在 turn 边界注入（不经 driver job 通道）
        .steering_queue(app.steering_handle())
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
        todo_store,
    )?;
    let mut term_events = EventStream::new();
    // spinner 帧推进：仅运行中需要动画，空闲时分支挂起不唤醒事件循环
    let mut spinner_ticker = tokio::time::interval(std::time::Duration::from_millis(100));
    // 光标形状随交互模式切换（vim 情境信号）：浏览态实心块，可键入态竖条
    let mut last_block_cursor = block_cursor(&app);
    set_cursor_style(last_block_cursor);
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
        if handle_wake(wake, &mut app, &mut driver, &mut terminal).await || app.should_quit() {
            break;
        }
        // QUEUE 的就地编辑子状态不改变模式字段，同样触发形状切换
        let block = block_cursor(&app);
        if block != last_block_cursor {
            last_block_cursor = block;
            set_cursor_style(block);
        }
    }
    Ok(())
}

/// 光标是否用实心块：NORMAL/VISUAL 与 QUEUE 导航子状态为实心块
///（不可键入文本的浏览态），其余竖条。
const fn block_cursor(app: &App) -> bool {
    match app.mode() {
        Mode::Normal | Mode::Visual => true,
        Mode::Queue => !app.queue_editing(),
        Mode::Insert | Mode::Search | Mode::Picker => false,
    }
}

/// 应用光标形状：实心块（浏览态）/ 竖条（可键入态）。
fn set_cursor_style(block: bool) {
    let style = if block {
        SetCursorStyle::SteadyBlock
    } else {
        SetCursorStyle::SteadyBar
    };
    let _ = execute!(io::stdout(), style);
}

/// 启动 agent driver：专属 tokio 任务持有 Agent，串行执行 job，完成后回传结果。
// 参数均为 driver 的独立组成部分，打包为参数结构只会增加间接层
#[allow(clippy::too_many_arguments)]
fn spawn_driver(
    agent: Agent,
    session: Option<(SessionStore, String)>,
    models: ModelResolver,
    model: Model,
    skill_resolver: SkillResolver,
    tip: Option<String>,
    reasoning: Option<ThinkingLevel>,
    todos: TodoStore,
) -> Result<(Driver, mpsc::UnboundedReceiver<DriverDone>)> {
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<DriverJob>();
    let (done_tx, done_rx) = mpsc::unbounded_channel::<DriverDone>();
    let driver_task = tokio::spawn(async move {
        let mut agent = agent;
        while let Some(job) = job_rx.recv().await {
            match job {
                DriverJob::Prompt(text, images, cancel) => {
                    let result = agent
                        .prompt_with_images(&text, &images, cancel.clone())
                        .await;
                    let done = match result {
                        Ok(messages) => {
                            // goal 模式追问判定：用户取消（Ctrl+C）或
                            // 响应以 Error/Aborted 收尾时不算正常结束
                            let last_stop =
                                messages.iter().rev().find_map(|message| match message {
                                    Message::Assistant(assistant) => Some(assistant.stop_reason),
                                    _ => None,
                                });
                            let ended_normally = !cancel.is_cancelled()
                                && !matches!(
                                    last_stop,
                                    Some(StopReason::Error | StopReason::Aborted)
                                );
                            Ok(PromptEnd { ended_normally })
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    if done_tx.send(DriverDone::Prompt(done)).is_err() {
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
                DriverJob::SwitchModel(switch) => {
                    // 先换 provider 再换模型：driver 串行处理，紧随的请求
                    // 一定跑在新 provider 的新模型上
                    if let Some(provider) = switch.provider {
                        agent.set_provider(provider.provider, provider.api_key);
                    }
                    agent.set_model(switch.model);
                }
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
        todos,
        goal_nudges: 0,
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
    /// todo 清单（与 agent 的 todo 工具共享；goal 模式追问判定用）
    todos: TodoStore,
    /// goal 模式连续自动追问次数（用户提交新 prompt 或 run 异常结束时清零）
    goal_nudges: u32,
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
async fn handle_wake(
    wake: Wake,
    app: &mut App,
    driver: &mut Driver,
    terminal: &mut TuiTerminal,
) -> bool {
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
                    execute_effect(app, driver, terminal, effect).await;
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
                handle_prompt_done(app, driver, terminal, result).await;
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

/// 一轮 prompt 结束：队列（ADR-0014）优先于 goal 模式追问——
/// 队列非空时，正常结束即取出队首自动提交（QUEUE 模式打开期间冻结，
/// 退出 QUEUE 时恢复）；被取消/失败等异常结束则队列暂停保留，
/// 用户可空闲 Enter 或 Esc→Q 恢复。队列为空时才走 goal 模式追问：
/// goal 开启、run 正常结束且仍有未完成 todo 时自动以 user 消息追问
///（连续次数有上限，防模型反复不收尾时失控循环）；其余情况回到空闲态。
/// 追问计数在用户提交新 prompt、run 异常结束或清单全部完成时清零。
async fn handle_prompt_done(
    app: &mut App,
    driver: &mut Driver,
    terminal: &mut TuiTerminal,
    result: Result<PromptEnd, String>,
) {
    let end = match result {
        Ok(end) => end,
        Err(error) => {
            driver.goal_nudges = 0;
            app.finish_run(Some(format!("agent loop 失败：{error}")));
            return;
        }
    };
    if app.queue_len() > 0 {
        driver.goal_nudges = 0;
        if end.ended_normally {
            app.finish_run(None);
            // QUEUE 模式打开时 drain 冻结（返回 None）：退出 QUEUE 时恢复
            if let Some(effect) = app.drain_queue() {
                execute_effect(app, driver, terminal, effect).await;
            }
        } else {
            app.finish_run(Some(format!(
                "运行未正常结束，队列保留 {} 条：空闲 Enter 发送下一条，Esc→Q 编辑",
                app.queue_len()
            )));
        }
        return;
    }
    let reminder = if end.ended_normally && app.goal_mode() {
        goal_reminder_prompt(&driver.todos)
    } else {
        None
    };
    let Some(reminder) = reminder else {
        driver.goal_nudges = 0;
        app.finish_run(None);
        return;
    };
    if driver.goal_nudges >= MAX_GOAL_NUDGES {
        driver.goal_nudges = 0;
        app.finish_run(Some(format!(
            "goal 模式：已连续追问 {MAX_GOAL_NUDGES} 次，todo 仍未全部完成，\
             暂停自动追问（手动继续或 /goal 重开）。"
        )));
        return;
    }
    driver.goal_nudges += 1;
    let token = CancellationToken::new();
    if driver
        .job_tx
        .send(DriverJob::Prompt(reminder, Vec::new(), token.clone()))
        .is_ok()
    {
        driver.current_cancel = Some(token);
        app.begin_run();
    } else {
        app.finish_run(Some(
            "内部错误：agent 任务已退出，goal 追问未发送。".to_string(),
        ));
    }
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
        // Alt+Enter 与 Enter 同义（统一消息队列，ADR-0014）
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
        (KeyCode::Char(c), KeyModifiers::ALT) => Key::Alt(c),
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => Key::Char(c),
        _ => return None,
    })
}

/// 执行 [`App::press`] 返回的语义效果：driver job、取消令牌、session 库、
/// skill resolver、图片加载、外部编辑器等外部资源在此接线。
async fn execute_effect(
    app: &mut App,
    driver: &mut Driver,
    terminal: &mut TuiTerminal,
    effect: Effect,
) {
    match effect {
        Effect::Prompt { text, images } => {
            // 用户主动提交：重置 goal 模式连续追问计数
            driver.goal_nudges = 0;
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
        Effect::OpenEditor => edit_input_in_editor(app, terminal).await,
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

/// `/models`：跨 provider 列出候选模型并打开选择器（预选中当前模型）。
fn list_models(app: &mut App, driver: &Driver) {
    let current = current_selection(&driver.model);
    let choices = driver.models.candidates(&current);
    if choices.is_empty() {
        // 理论不可达（候选至少含内置 provider 的默认模型），防御配置在运行期失效
        app.warn("没有可用的模型候选");
        return;
    }
    let selected = choices
        .iter()
        .position(|choice| is_current(choice, &current))
        .unwrap_or(0);
    let rows = choices
        .iter()
        .map(|choice| PickerRow {
            id: choice.spec(),
            text: model_row_text(choice, &current),
            selectable: true,
        })
        .collect();
    app.open_model_picker(rows, selected);
}

/// 候选行是否为当前模型（provider 与模型 id 均相同）。
fn is_current(choice: &ModelChoice, current: &ModelSelection) -> bool {
    (choice.provider.as_str(), choice.id.as_str())
        == (current.provider.as_str(), current.model.as_str())
}

/// 当前模型的选择项（`<provider>/<模型id>`）。
fn current_selection(model: &Model) -> ModelSelection {
    ModelSelection {
        provider: model.provider.clone(),
        model: model.id.clone(),
    }
}

/// 选择器行文本：`<provider>/<模型id> — 展示名 · ctx 200k · 支持思考`，
/// 当前模型带标记；窗口未知省略 ctx。
fn model_row_text(choice: &ModelChoice, current: &ModelSelection) -> String {
    use std::fmt::Write as _;
    let mut text = format!("{} — {}", choice.spec(), choice.name);
    if choice.context_window > 0 {
        let _ = write!(text, " · ctx {}", ui::format_tokens(choice.context_window));
    }
    if choice.reasoning {
        text.push_str(" · 支持思考");
    }
    if is_current(choice, current) {
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
        parts.push(switched_part(&driver.model));
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

/// `/models:<p>/<id>` 或模型选择器确认：先选模型后选 effort——
///
/// - 目标模型支持推理：暂存为待切换模型并打开思考级别选择器（流程第二步）；
///   确认级别时一并应用切换，Esc 放弃整个切换。重选当前模型时不暂存，
///   级别选择器变为单纯的级别调整入口
/// - 目标模型不支持推理：直接切换（级别设置保留但随请求被忽略，
///   与配置文件 `reasoning` 同一口径）
///
/// 选择项为 `<provider>/<模型id>` 全形式；裸模型 id 在当前 provider 内解析。
fn select_model(app: &mut App, driver: &mut Driver, id: &str) {
    let selection = match ModelSelection::parse(id, Some(&driver.model.provider)) {
        Ok(selection) => selection,
        Err(error) => {
            app.warn(format!("切换模型失败：{error:#}"));
            return;
        }
    };
    match driver.models.resolve(&selection.provider, &selection.model) {
        Err(error) => app.warn(format!("切换模型失败：{error:#}")),
        Ok(model) if model.reasoning => {
            driver.pending_model = (!same_model(&model, &driver.model)).then_some(model);
            open_reasoning_picker(app, driver);
        }
        Ok(model) if same_model(&model, &driver.model) => {
            app.push_system(format!("当前模型已是 {}（不支持思考）。", model.name));
        }
        Ok(model) => {
            if apply_model_switch(app, driver, model) {
                app.push_system(switch_notice(&driver.model));
            }
        }
    }
}

/// 同一模型判断：provider 与模型 id 均相同。
fn same_model(a: &Model, b: &Model) -> bool {
    a.provider == b.provider && a.id == b.id
}

/// 发送 SwitchModel job 并同步 driver/app 状态（状态栏徽标、上下文窗口）；
/// 成功返回 `true`，driver 已退出时告警并返回 `false`。
///
/// 跨 provider 时一并构造新连接实现（api_key 分层：环境变量 >
/// `providers.<名字>.api_key` > 平铺配置）；切换成功后把选择追加到
/// sqlite 配置表（下次启动的回退链顶端）。driver 串行处理任务，紧随的
/// 级别设置与 prompt 一定跑在新模型上。
fn apply_model_switch(app: &mut App, driver: &mut Driver, model: Model) -> bool {
    let provider = (model.provider != driver.model.provider).then(|| {
        let api_key = bootstrap::resolve_api_key(
            None,
            std::env::var(bootstrap::api_key_env(model.api))
                .ok()
                .as_deref(),
            driver
                .models
                .provider_config(&model.provider)
                .and_then(|p| p.api_key.as_deref()),
            driver.models.config().and_then(|c| c.api_key.as_deref()),
        );
        ProviderSwitch {
            provider: bootstrap::build_provider(model.api, api_key.clone()),
            api_key,
        }
    });
    if driver
        .job_tx
        .send(DriverJob::SwitchModel(ModelSwitch {
            model: model.clone(),
            provider,
        }))
        .is_err()
    {
        app.warn("内部错误：agent 任务已退出，无法切换模型");
        return false;
    }
    persist_model_selection(driver, &model);
    let name = model.name.clone();
    let window = model.context_window;
    driver.model = model;
    app.set_model(name, window);
    true
}

/// 模型选择落库（config 表 append-only，最新行即下次启动的首选）。
///
/// 库不可用（启动已告警）时跳过；写失败只记日志不打断切换——
/// 下次启动的回退链只是少了这一条。
fn persist_model_selection(driver: &Driver, model: &Model) {
    let Some((store, _)) = &driver.session else {
        return;
    };
    let store = store.clone();
    let spec = current_selection(model).spec();
    tokio::spawn(async move {
        if let Err(error) = store
            .set_config(
                bootstrap::CONFIG_KEY_MODEL,
                &serde_json::Value::String(spec),
            )
            .await
        {
            tracing::warn!(error = %error, "模型选择落库失败");
        }
    });
}

/// 切换成功的提示文本：`已切换模型为 <provider>/<模型id>（名称 · ctx 400k），
/// 对话上下文保留。`
fn switch_notice(model: &Model) -> String {
    format!("{}，对话上下文保留。", switched_part(model))
}

/// 提示文本的模型切换段：`已切换模型为 <provider>/<模型id>（名称 · ctx 400k）`；
/// 名称与模型 id 相同、窗口未知时省略对应段。
fn switched_part(model: &Model) -> String {
    use std::fmt::Write as _;
    let mut text = format!("已切换模型为 {}", current_selection(model).spec());
    let mut detail = String::new();
    if model.name != model.id {
        detail.push_str(&model.name);
    }
    if model.context_window > 0 {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(detail, "ctx {}", ui::format_tokens(model.context_window));
    }
    if !detail.is_empty() {
        let _ = write!(text, "（{detail}）");
    }
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
            // 预选中当前分支末端；末端不可选（工具结果，或已被折叠进摘要行）
            // 时退到首个可选行
            let selected = driver
                .tip
                .as_deref()
                .and_then(|tip| rows.iter().position(|row| row.id == tip))
                .filter(|&index| rows[index].selectable)
                .or_else(|| rows.iter().position(|row| row.selectable))
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

/// 会话树条目 → 选择器行。
///
/// 缩进语义：**只在真实分叉处缩进**——用树形前缀（`├─`/`└─`/`│`）画出
/// 分支结构，线性链（含工具调用轮次）平铺。工具调用循环是父子链而非
/// 分支，若按祖先链长度缩进会把单线对话画成向右无限延伸的阶梯。
///
/// 不可选条目（含工具调用的 assistant 响应、工具结果）只是浏览上下文
/// 而非分支起点：连续的一段折叠为一行摘要（`↳ 工具调用 ×N（…）`），
/// 避免工具噪音淹没可选条目。折叠只取链上条目（子节点 ≤ 1），不会吞掉
/// 分叉点。
fn tree_rows(entries: &[TreeEntry], tip: Option<&str>) -> Vec<PickerRow> {
    let index: std::collections::HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.id.as_str(), i))
        .collect();
    // 每个 entry 的子节点（按插入序）：判定分叉与折叠用
    let mut children: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(parent) = entry.parent_id.as_deref() {
            children.entry(parent).or_default().push(i);
        }
    }
    // 树形前缀：沿父链收集「分叉边」（父节点有多个孩子的边）。entry 自己
    // 的分叉边渲染为 `├─ `/`└─ `；祖先的分叉边自外向内渲染为 `│  `/`   `
    // 层级。线性边不产生前缀。
    let prefix = |entry: &TreeEntry| {
        let mut ancestors: Vec<bool> = Vec::new(); // 祖先分叉边：该侧是否最末孩子
        let mut own: Option<bool> = None; // 自身边是否分叉
        let mut child = entry.id.as_str();
        let mut cursor = entry.parent_id.as_deref();
        let mut first = true;
        // 父指针在写入时校验存在，父链必然终止于根；缺失数据按根处理
        while let Some(parent) = cursor {
            let siblings = children.get(parent).map_or(&[][..], Vec::as_slice);
            if siblings.len() > 1 {
                let last = siblings.last().copied() == Some(index[child]);
                if first {
                    own = Some(last);
                } else {
                    ancestors.push(last);
                }
            }
            first = false;
            child = parent;
            cursor = index
                .get(parent)
                .and_then(|&i| entries[i].parent_id.as_deref());
        }
        let mut text = String::new();
        for &last in ancestors.iter().rev() {
            text.push_str(if last { "   " } else { "│  " });
        }
        if let Some(last) = own {
            text.push_str(if last { "└─ " } else { "├─ " });
        }
        text
    };
    // 可折叠：不可选且位于链上（子节点 ≤ 1）；有多子节点的条目是分叉点，
    // 必须保留原行以呈现树形结构
    let foldable = |entry: &TreeEntry| {
        !entry.is_branchable() && children.get(entry.id.as_str()).map_or(0, Vec::len) <= 1
    };
    let mut rows = Vec::with_capacity(entries.len());
    let mut i = 0;
    while i < entries.len() {
        if !foldable(&entries[i]) {
            rows.push(entry_row(&entries[i], tip, &prefix(&entries[i])));
            i += 1;
            continue;
        }
        let start = i;
        while i + 1 < entries.len() && foldable(&entries[i + 1]) {
            i += 1;
        }
        rows.push(fold_row(&entries[start..=i], tip, &prefix(&entries[start])));
        i += 1;
    }
    rows
}

/// 单条目的选择器行：树形前缀 + 角色/时间/预览，当前分支末端带标记。
fn entry_row(entry: &TreeEntry, tip: Option<&str>, prefix: &str) -> PickerRow {
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
            "{prefix}{role} · {} · {}{current}",
            crate::sessions::format_time(Some(entry.timestamp)),
            entry.preview,
        ),
        selectable: entry.is_branchable(),
    }
}

/// 一段连续工具条目的折叠摘要行（不可选，仅浏览上下文）。
///
/// 工具名与失败数从工具结果 preview（`工具结果：{name}` / `工具失败：{name}`，
/// 见 nomic-session 的 `message_preview`）统计；preview 无法解析（如 payload
/// 损坏的占位文本）只计入总数。run 内含当前分支末端（运行中打开 `/tree`）
/// 时带标记。
fn fold_row(run: &[TreeEntry], tip: Option<&str>, prefix: &str) -> PickerRow {
    let mut calls = 0_usize;
    let mut failures = 0_usize;
    let mut tools: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for entry in run.iter().filter(|entry| entry.role == "tool_result") {
        calls += 1;
        if let Some(name) = entry.preview.strip_prefix("工具结果：") {
            *tools.entry(name).or_default() += 1;
        } else if let Some(name) = entry.preview.strip_prefix("工具失败：") {
            failures += 1;
            *tools.entry(name).or_default() += 1;
        }
    }
    let mut parts: Vec<String> = tools
        .iter()
        .map(|(name, count)| format!("{name} ×{count}"))
        .collect();
    if failures > 0 {
        parts.push(format!("失败 ×{failures}"));
    }
    let stats = if parts.is_empty() {
        String::new()
    } else {
        format!("（{}）", parts.join(" · "))
    };
    // 无工具结果（如运行被打断）：退化为条目计数
    let summary = if calls > 0 {
        format!("工具调用 ×{calls}{stats}")
    } else {
        format!("工具调用 ×{}（无结果）", run.len())
    };
    let current = if run.iter().any(|entry| Some(entry.id.as_str()) == tip) {
        "（当前）"
    } else {
        ""
    };
    PickerRow {
        id: run[0].id.clone(),
        text: format!(
            "{prefix}↳ {summary} · {}{current}",
            crate::sessions::format_time(Some(run[0].timestamp)),
        ),
        selectable: false,
    }
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

/// INSERT `Ctrl+G`：挂起 TUI，用系统编辑器编辑当前输入缓冲，退出后
/// 恢复终端并把结果写回（写回语义见 [`App::apply_editor_result`]）。
///
/// 编辑器运行期间事件循环挂起是本意的同步语义：tty 已交给编辑器，
/// TUI 不应重绘；crossterm 的 EventStream 后台线程只 poll 就绪不读
/// 字节（0.29 起 read 发生在消费侧 poll_next），不轮询就不会与编辑器
/// 争抢 stdin，编辑器里的按键不会漏回 TUI。agent 运行不受影响
///（driver 是独立任务），期间到的事件在 channel 里积压，恢复后照常处理。
async fn edit_input_in_editor(app: &mut App, terminal: &mut TuiTerminal) {
    let initial = app.input().to_string();
    leave_tui_terminal();
    // spawn_blocking 与剪贴板同一口径：编辑器可能运行很久，不占 runtime worker
    let outcome = tokio::task::spawn_blocking(move || run_external_editor(&initial)).await;
    // 恢复失败也只是渲染异常，编辑结果照常写回
    if let Err(error) = enter_tui_terminal() {
        app.warn(format!("恢复终端失败：{error}"));
    }
    // 离开期间缓冲区已与屏幕脱节：清屏强制下一帧全量重绘
    let _ = terminal.clear();
    // leave 时还原了用户惯用光标形状，按当前模式重新应用
    set_cursor_style(block_cursor(app));
    match outcome {
        Ok(Ok(text)) => app.apply_editor_result(&text),
        Ok(Err(error)) => app.warn(format!("{error:#}")),
        Err(join) => app.warn(format!("打开编辑器失败：{join}")),
    }
}

/// 在临时文件上运行系统编辑器，返回编辑后的内容。
///
/// 编辑器解析：`$VISUAL` → `$EDITOR` → `vi`（与 git 同一口径）；命令
/// 经 `sh -c` 执行以支持带参数形式（如 `code --wait`）。退出码非 0
///（如 vim `:cq`）视为放弃编辑：报错且调用方保留原草稿。临时文件
/// 带 `.md` 后缀让编辑器启用 markdown 高亮，随 [`tempfile::NamedTempFile`]
///  drop 自动删除。
fn run_external_editor(initial: &str) -> Result<String> {
    use std::io::Write as _;

    let mut file = tempfile::Builder::new()
        .prefix("nomic-prompt-")
        .suffix(".md")
        .tempfile()
        .context("创建临时文件失败")?;
    file.write_all(initial.as_bytes())
        .context("写入临时文件失败")?;
    file.flush().context("flush 临时文件失败")?;
    let path = file.path().to_path_buf();

    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "vi".to_string());
    // 编辑器命令交 sh 解析（与 git 的 GIT_EDITOR 口径一致），支持
    // "code --wait" 等带参数形式；`$@` 展开为临时文件路径
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("启动编辑器 {editor:?} 失败"))?;
    anyhow::ensure!(
        status.success(),
        "编辑器 {editor:?} 异常退出（{status}），输入未变"
    );

    std::fs::read_to_string(&path).context("读取编辑结果失败")
}

/// `/copy`：把文本写入系统剪贴板。
///
/// 与粘贴同理，写入可能阻塞在 X11/Wayland 往返上，放 `spawn_blocking` 中执行。
async fn copy_to_clipboard(app: &mut App, text: String) {
    let chars = text.chars().count();
    match tokio::task::spawn_blocking(move || crate::clipboard::write_text(&text)).await {
        Ok(Ok(())) => app.push_system(format!("已复制到剪贴板（{chars} 字）。")),
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

/// 终端状态守卫：进入 TUI 终端态；Drop（含 panic 路径经 hook）时恢复。
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enter_tui_terminal()?;
        install_panic_hook();
        Ok(Self)
    }

    fn restore() {
        leave_tui_terminal();
    }
}

/// 进入 TUI 终端态：raw mode + alternate screen + 鼠标捕获 +
/// bracketed paste + kitty 键盘增强。启动（[`TerminalGuard`]）与
/// 外部编辑器退出后的恢复共用，保证两处口径一致。
fn enter_tui_terminal() -> io::Result<()> {
    enable_raw_mode()?;
    // bracketed paste：终端粘贴/拖入的内容整体作为 Event::Paste 上报，
    // 便于识别图片路径；不支持的终端忽略该序列，退化为逐键事件。
    // 鼠标捕获用于滚轮滚动聊天区；代价是终端原生文本选择被劫持，
    // 用户需按住 Shift 拖选（README 与欢迎页已说明）
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
    Ok(())
}

/// 离开 TUI 终端态，把 tty 还给 shell/外部编辑器：恢复 cooked 模式、
/// 退出 alternate screen、关鼠标捕获与 bracketed paste、弹键盘增强、
/// 还原用户惯用光标形状（NORMAL 的实心块不残留到 shell）。
/// 尽力而为：单项失败不中断后续恢复步骤（退出路径不能卡在半恢复态）。
fn leave_tui_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags,
        SetCursorStyle::DefaultUserShape
    );
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
        ModelChoice, ModelSelection, ReasoningSetting, goal_reminder_prompt, model_row_text,
        panic_payload_text, paste_image_path, percent_decode, reasoning_label, reasoning_row_text,
        reasoning_setting, tree_rows,
    };
    use nomic_ai::ThinkingLevel;
    use nomic_core::AgentTool;
    use nomic_session::TreeEntry;
    use nomic_tools::{TodoItemInput, TodoStatus, TodoStore, TodoWriteTool};

    /// goal 模式追问提示词：列出未完成 todo；空清单或全部完成时不追问。
    #[tokio::test]
    async fn goal_reminder_lists_incomplete_todos() {
        async fn write(store: &TodoStore, todos: Vec<TodoItemInput>) {
            let tool = TodoWriteTool::new(store.clone());
            tool.execute(
                nomic_tools::TodoWriteParams { todos },
                tokio_util::sync::CancellationToken::new(),
                Box::new(|_| {}),
            )
            .await
            .expect("写入应成功");
        }
        let item = |title: &str, status: TodoStatus| TodoItemInput {
            id: None,
            title: title.to_string(),
            status,
            children: Vec::new(),
        };

        // 空清单：不追问
        let store = TodoStore::new();
        assert_eq!(goal_reminder_prompt(&store), None);

        // 有未完成项：提示词列出 pending / in_progress，不含 completed
        write(
            &store,
            vec![
                item("修复测试", TodoStatus::Completed),
                item("更新文档", TodoStatus::Pending),
                item("补充单测", TodoStatus::InProgress),
            ],
        )
        .await;
        let prompt = goal_reminder_prompt(&store).expect("有未完成项应追问");
        assert!(prompt.contains("[goal 模式]"), "{prompt}");
        assert!(prompt.contains("更新文档"), "{prompt}");
        assert!(prompt.contains("补充单测"), "{prompt}");
        assert!(!prompt.contains("修复测试"), "{prompt}");

        // 全部完成（含已取消）：不追问
        write(
            &store,
            vec![
                item("修复测试", TodoStatus::Completed),
                item("过时任务", TodoStatus::Cancelled),
            ],
        )
        .await;
        assert_eq!(goal_reminder_prompt(&store), None);
    }

    /// 会话树选择器行：线性链（含工具调用轮次）平铺不缩进，连续工具条目
    /// 折叠为一行摘要（不可选），当前分支末端带标记。
    #[test]
    fn tree_rows_flatten_linear_chain_and_fold_tools() {
        let entry = |id: &str, parent: Option<&str>, role: &str, tool_calls: bool| TreeEntry {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            role: role.to_string(),
            timestamp: 1_785_000_000_000,
            preview: format!("preview of {id}"),
            has_tool_calls: tool_calls,
        };
        let tool_result = |id: &str, parent: &str, name: &str, failed: bool| {
            let mut entry = entry(id, Some(parent), "tool_result", false);
            entry.preview = if failed {
                format!("工具失败：{name}")
            } else {
                format!("工具结果：{name}")
            };
            entry
        };
        let entries = vec![
            entry("root", None, "user", false),
            entry("a1", Some("root"), "assistant", true),
            tool_result("t1", "a1", "bash", false),
            tool_result("t2", "t1", "bash", true),
            entry("a2", Some("t2"), "assistant", false),
        ];

        let rows = tree_rows(&entries, Some("t2"));
        assert_eq!(rows.len(), 3, "工具条目折叠为一行：{rows:?}");
        assert!(rows[0].text.starts_with("用户 · "), "{}", rows[0].text);
        assert!(
            rows[1]
                .text
                .starts_with("↳ 工具调用 ×2（bash ×2 · 失败 ×1）"),
            "{}",
            rows[1].text
        );
        assert!(rows[1].text.ends_with("（当前）"), "{}", rows[1].text);
        assert!(
            rows[2].text.starts_with("助手 · "),
            "线性链不缩进：{}",
            rows[2].text
        );

        assert!(rows[0].selectable);
        assert!(!rows[1].selectable, "折叠摘要行不可选");
        assert!(rows[2].selectable);
    }

    /// 会话树选择器行：真实分叉用树形前缀（`├─`/`└─`/`│`）画分支结构，
    /// 分叉下的线性后代继承层级前缀。
    #[test]
    fn tree_rows_draw_branch_prefixes_at_forks() {
        let entry = |id: &str, parent: Option<&str>| TreeEntry {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            role: "user".to_string(),
            timestamp: 1_785_000_000_000,
            preview: format!("preview of {id}"),
            has_tool_calls: false,
        };
        let entries = vec![
            entry("root", None),
            entry("b1", Some("root")),
            entry("c1", Some("b1")),
            entry("b2", Some("root")),
            entry("c2", Some("b2")),
        ];

        let rows = tree_rows(&entries, Some("c2"));
        assert_eq!(rows.len(), 5);
        assert!(rows[0].text.starts_with("用户 · "), "{}", rows[0].text);
        assert!(rows[1].text.starts_with("├─ "), "{}", rows[1].text);
        assert!(
            rows[2].text.starts_with("│  "),
            "非最末分支的后代画竖线：{}",
            rows[2].text
        );
        assert!(rows[3].text.starts_with("└─ "), "{}", rows[3].text);
        assert!(
            rows[4].text.starts_with("   "),
            "最末分支的后代留白：{}",
            rows[4].text
        );
        assert!(rows[4].text.ends_with("（当前）"), "{}", rows[4].text);
        assert!(rows.iter().all(|row| row.selectable));
    }

    /// 折叠不吞分叉点：不可选条目若有多个子节点（历史数据防御），保留原行。
    #[test]
    fn tree_rows_keep_unselectable_fork_point() {
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
            entry("b1", Some("a1"), "user", false),
            entry("b2", Some("a1"), "user", false),
        ];

        let rows = tree_rows(&entries, None);
        assert_eq!(rows.len(), 4, "分叉点不折叠：{rows:?}");
        assert!(!rows[1].selectable, "含工具调用的 assistant 条目不可选");
        assert!(rows[2].text.starts_with("├─ "), "{}", rows[2].text);
        assert!(rows[3].text.starts_with("└─ "), "{}", rows[3].text);
    }

    /// `/models` 选择器行：id + 展示名 + 窗口，推理模型带标注，当前模型带标记，
    /// 窗口未知省略 ctx。
    #[test]
    fn model_row_text_formats_window_and_marks_current() {
        let choice = ModelChoice {
            provider: "openai".to_string(),
            id: "gpt-5.2".to_string(),
            name: "GPT-5.2".to_string(),
            context_window: 400_000,
            reasoning: true,
        };
        let current = ModelSelection::parse("openai/gpt-5.2", None).unwrap();
        assert_eq!(
            model_row_text(&choice, &current),
            "openai/gpt-5.2 — GPT-5.2 · ctx 400k · 支持思考（当前）"
        );
        let other = ModelSelection::parse("openai/other", None).unwrap();
        assert_eq!(
            model_row_text(&choice, &other),
            "openai/gpt-5.2 — GPT-5.2 · ctx 400k · 支持思考"
        );
        // 同名模型 id 但 provider 不同：不是当前模型
        let other_provider = ModelSelection::parse("deepseek/gpt-5.2", None).unwrap();
        assert!(!model_row_text(&choice, &other_provider).contains("（当前）"));
        let no_thinking = ModelChoice {
            reasoning: false,
            ..choice
        };
        assert_eq!(
            model_row_text(&no_thinking, &other),
            "openai/gpt-5.2 — GPT-5.2 · ctx 400k"
        );
        let unknown = ModelChoice {
            provider: "openai".to_string(),
            id: "m".to_string(),
            name: "m".to_string(),
            context_window: 0,
            reasoning: false,
        };
        assert_eq!(model_row_text(&unknown, &other), "openai/m — m");
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
