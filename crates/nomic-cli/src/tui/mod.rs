//! 交互 TUI（ratatui + crossterm，设计见 docs/adr/0002）。
//!
//! 结构：
//! - [`app`]：纯状态层——对外为语义操作（按键 [`app::Key`] → [`app::Effect`]、
//!   应用事件、滚动、会话/附件管理），脱离终端可测；内部按关注点拆为
//!   chat（条目 + delta 累积 + 滚动）、input（草稿 + 编辑 + 补全）、
//!   queue（统一消息队列与 QUEUE 模式）、picker/search 子模块，`App`
//!   只做组合与模式路由；INSERT `Ctrl+G` 外部编辑器（ADR-0017）由本文件
//!   [`edit_input_in_editor`] 接线，状态层只消费写回结果
//! - [`effects`]：Effect 执行逻辑，按族分组为子模块——`model`
//!   （模型 + 思考级别两步流）、`session`（resume / tree / branch /
//!   new 与落库）、`clipboard`（粘贴 / 复制 / 图片暂存）
//! - [`ui`]：纯渲染（聊天区 + 输入框 + 状态栏）
//! - 本文件：终端生命周期、事件循环（`KeyEvent` → `Key` 映射、`Effect` 转发执行）、
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
mod effects;
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
use nomic_session::SessionStore;
use nomic_skills::SkillResolver;
use nomic_tools::TodoStore;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use app::{App, Effect, Key, Mode, SkillEntry};

use crate::model::ModelResolver;
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
    effects::stage_cli_images(&mut app, &cli.image);
    let skill_resolver = boot.skill_resolver.clone();
    app.input_mut().set_available_skills(
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
    app.input_mut()
        .set_available_templates(boot.prompt_templates.clone());
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
        .steering_queue(app.queue().handle())
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
            &app,
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

/// 光标是否用实心块：NORMAL/VISUAL/HELP 与 QUEUE 导航子状态为实心块
///（不可键入文本的浏览态）。
const fn block_cursor(app: &App) -> bool {
    match app.mode() {
        Mode::Normal | Mode::Visual | Mode::Help => true,
        Mode::Queue => !app.queue().is_editing(),
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
    /// 仅需重绘（其他鼠标事件）
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
                effects::paste_clipboard(app).await;
            } else if let Some(key) = map_key(key) {
                for effect in app.press(key) {
                    execute_effect(app, driver, terminal, effect).await;
                }
            }
        }
        Wake::ScrollUp => app.chat_mut().scroll_up(3),
        Wake::ScrollDown => app.chat_mut().scroll_down(3),
        Wake::Paste(text) => effects::handle_paste(app, &text),
        Wake::AgentEvent(event) => {
            match &event {
                AgentEvent::MessageEnd(message) => {
                    effects::persist(driver, message, app).await;
                }
                AgentEvent::CompactionEnd {
                    summary,
                    tokens_before,
                    kept_count,
                    ..
                } => {
                    effects::persist_compaction(driver, summary, *tokens_before, *kept_count, app)
                        .await;
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
            app.chat_mut().push_system(format!(
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
    if !app.queue().is_empty() {
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
                app.queue().len()
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
    app: &App,
    driver: &mut Driver,
    term_events: &mut EventStream,
    spinner_ticker: &mut tokio::time::Interval,
    events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    done_rx: &mut mpsc::UnboundedReceiver<DriverDone>,
) -> Wake {
    let driver_alive = driver.alive;
    let running = app.is_running();
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
/// skill resolver、图片加载等外部资源在此接线。
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
        // INSERT `Ctrl+G`：挂起 TUI 运行外部编辑器（ADR-0017），退出后写回
        Effect::OpenEditor => edit_input_in_editor(app, terminal).await,
        Effect::ListSessions => effects::list_sessions(app, driver).await,
        Effect::Resume(id) => {
            effects::resume_session(app, driver, id).await;
        }
        Effect::ListTree => effects::list_tree(app, driver).await,
        Effect::BranchTo(entry_id) => effects::branch_to(app, driver, entry_id).await,
        Effect::ListModels => effects::list_models(app, driver),
        Effect::SwitchModel(id) => effects::select_model(app, driver, &id),
        Effect::SetReasoning(level) => effects::set_reasoning(app, driver, &level),
        Effect::CancelModelSwitch => effects::cancel_model_switch(app, driver),
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
        Effect::LoadSkill(invocation) => {
            match driver.skill_resolver.activate(&invocation.name) {
                Ok(skill) => {
                    // 注入消息经事件管线回流：聊天区压缩展示 + session 落库自动生效
                    let _ = driver
                        .job_tx
                        .send(DriverJob::Inject(app::skill_load_message(
                            &skill,
                            invocation.args.as_deref(),
                        )));
                }
                Err(error) => app.warn(format!("载入 skill {:?} 失败：{error}", invocation.name)),
            }
        }
        Effect::AttachImage(path) => effects::attach_image(app, &std::path::PathBuf::from(path)),
        Effect::CopyText(text) => effects::copy_to_clipboard(app, text).await,
        Effect::NewSession => effects::new_session(app, driver).await,
    }
}

/// INSERT `Ctrl+G`：挂起 TUI，用外部编辑器编辑当前输入缓冲，退出后
/// 恢复终端并把结果写回（写回语义见 [`App::apply_editor_result`]）。
///
/// 编辑器运行期间事件循环挂起是本意的同步语义：tty 已交给编辑器，
/// TUI 不应重绘；crossterm 的 EventStream 后台线程只 poll 就绪不读
/// 字节（0.29 起 read 发生在消费侧 poll_next），不轮询就不会与编辑器
/// 争抢 stdin，编辑器里的按键不会漏回 TUI。agent 运行不受影响
///（driver 是独立任务），期间到的事件在 channel 里积压，恢复后照常处理。
async fn edit_input_in_editor(app: &mut App, terminal: &mut TuiTerminal) {
    let initial = app.input().text().to_string();
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

/// 在临时文件上运行外部编辑器，返回编辑后的内容。
///
/// 编辑器解析：`$VISUAL` → `$EDITOR` → `vi`（与 git 同一口径）；命令
/// 经 `sh -c` 执行以支持带参数形式（如 `code --wait`）。退出码非 0
///（如 vim `:cq`）视为放弃编辑：报错且调用方保留原草稿。临时文件
/// 带 `.md` 后缀让编辑器启用 markdown 高亮，随 [`tempfile::NamedTempFile`]
/// drop 自动删除。
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

    use super::{goal_reminder_prompt, panic_payload_text};
    use nomic_core::AgentTool;
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

    #[test]
    fn panic_payload_extracts_message() {
        let payload: Box<dyn Any + Send> = Box::new("boom");
        assert_eq!(panic_payload_text(&*payload), "boom");

        let payload: Box<dyn Any + Send> = Box::new("owned boom".to_string());
        assert_eq!(panic_payload_text(&*payload), "owned boom");

        let payload: Box<dyn Any + Send> = Box::new(42_i32);
        assert_eq!(panic_payload_text(&*payload), "未知负载");
    }
}
