//! 交互 TUI（ratatui + crossterm，设计见 docs/adr/0002）。
//!
//! 结构：
//! - [`app`]：纯状态层（聊天条目、流式累积、输入编辑、滚动），脱离终端可测
//! - [`ui`]：纯渲染（聊天区 + 输入框 + 状态栏）
//! - 本文件：终端生命周期、事件循环、agent driver 任务
//!
//! agent 由专属 tokio 任务持有（`Agent::prompt` 需要 `&mut self` 且跨轮复用），
//! TUI 经 mpsc 发送 prompt（附本轮 `CancellationToken`），agent 事件经既有
//! channel 回流；`MessageEnd` 定稿点复用事件驱动落库。

mod app;
mod markdown;
mod theme;
mod ui;

use std::io;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseEventKind,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use futures::StreamExt as _;
use nomic_ai::Message;
use nomic_core::{Agent, AgentConfig, AgentEvent, Compaction, ExecutionMode, NoopHooks};
use nomic_session::{CompactionRecord, SessionStore};
use nomic_skills::SkillResolver;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use app::{App, SkillEntry, SlashAction, SlashParse};

use crate::{Cli, bootstrap};

/// 提交给 agent driver 的任务。
enum DriverJob {
    /// 运行一轮 prompt（附本轮取消令牌）
    Prompt(String, CancellationToken),
    /// 手动压缩上下文（`/compact [聚焦指令]`，附本轮取消令牌）
    Compact(Option<String>, CancellationToken),
    /// 向 agent 历史注入一条 user 消息（`/skill:<name>` 手动载入），不启动 run
    Inject(String),
    /// 清空 agent 上下文（`/new`）
    Clear,
    /// 整体替换 agent 上下文（`/resume` 恢复历史 session）
    Restore(Vec<Message>),
}

/// agent driver 完成的任务回执。
enum DriverDone {
    /// 一轮 prompt 结束（Err 为 agent loop 错误）
    Prompt(Result<(), String>),
    /// 一次手动压缩结束（Ok(None) 表示无可压缩内容；Err 为摘要失败）
    Compact(Result<Option<Compaction>, String>),
}

/// 运行交互 TUI。
pub async fn run(cli: &Cli) -> Result<()> {
    let boot = bootstrap::bootstrap(cli).await?;

    let mut app = App::new(
        boot.model.name.clone(),
        boot.session.as_ref().map(|(_, id)| id.clone()),
    );
    app.load_history(&boot.history);
    let skill_resolver = boot.skill_resolver.clone();
    app.set_available_skills(
        skill_resolver
            .catalog()
            .into_iter()
            .map(|skill| SkillEntry {
                name: skill.name,
                description: skill.document.description,
            })
            .collect(),
    );

    let (agent, mut events) = Agent::with_messages(
        AgentConfig {
            model: boot.model,
            provider: boot.provider,
            stream_options: boot.stream_options,
            hooks: Arc::new(NoopHooks),
            tool_execution: ExecutionMode::Parallel,
            compaction: boot.compaction,
        },
        nomic_tools::default_tools_with_skills(boot.skill_resolver),
        boot.system_prompt,
        boot.history,
    );

    let _guard = TerminalGuard::enter().context("初始化终端失败")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("创建终端后端失败")?;

    // agent driver：持有 Agent，串行执行 prompt，完成后回传结果
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<DriverJob>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<DriverDone>();
    tokio::spawn(async move {
        let mut agent = agent;
        while let Some(job) = job_rx.recv().await {
            match job {
                DriverJob::Prompt(text, cancel) => {
                    let result = agent
                        .prompt(&text, cancel)
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
                DriverJob::Inject(text) => agent.inject_user_message(&text),
                DriverJob::Clear => agent.clear_messages(),
                DriverJob::Restore(messages) => agent.restore_messages(messages),
            }
        }
    });

    let mut term_events = EventStream::new();
    // spinner 帧推进：仅运行中需要动画，空闲时分支挂起不唤醒事件循环
    let mut spinner_ticker = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut driver = Driver {
        job_tx,
        current_cancel: None,
        session: boot.session,
        cwd: std::env::current_dir().context("get cwd")?,
        skill_resolver,
    };
    loop {
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .context("绘制失败")?;
        let wake = next_wake(
            app.running,
            &mut term_events,
            &mut spinner_ticker,
            &mut events,
            &mut done_rx,
        )
        .await;
        if handle_wake(wake, &mut app, &mut driver).await || app.should_quit {
            break;
        }
    }
    Ok(())
}

/// 事件循环持有的驱动端资源。
struct Driver {
    job_tx: mpsc::UnboundedSender<DriverJob>,
    current_cancel: Option<CancellationToken>,
    session: Option<(SessionStore, String)>,
    cwd: std::path::PathBuf,
    skill_resolver: SkillResolver,
}

/// 事件循环单次等待的结果。
enum Wake {
    /// 按键（Press/Repeat）
    Key(KeyEvent),
    /// 鼠标滚轮
    ScrollUp,
    ScrollDown,
    /// agent 事件
    AgentEvent(AgentEvent),
    /// driver 任务完成（prompt 或手动压缩）
    AgentDone(DriverDone),
    /// spinner 帧推进
    Tick,
    /// 仅需重绘（resize、其他鼠标事件）
    Redraw,
    /// 任一事件流关闭：退出循环
    Closed,
}

/// 处理一次唤醒；返回 `true` 表示事件流关闭、退出循环。
async fn handle_wake(wake: Wake, app: &mut App, driver: &mut Driver) -> bool {
    match wake {
        Wake::Key(key) => {
            handle_key(
                app,
                &mut driver.current_cancel,
                &driver.job_tx,
                &mut driver.session,
                &driver.cwd,
                &driver.skill_resolver,
                key,
            )
            .await;
        }
        Wake::ScrollUp => app.scroll_up(3),
        Wake::ScrollDown => app.scroll_down(3),
        Wake::AgentEvent(event) => {
            match &event {
                AgentEvent::MessageEnd(message) => {
                    persist(driver.session.as_ref(), message, app).await;
                }
                AgentEvent::CompactionEnd {
                    summary,
                    tokens_before,
                    kept_count,
                    ..
                } => {
                    persist_compaction(
                        driver.session.as_ref(),
                        summary,
                        *tokens_before,
                        *kept_count,
                        app,
                    )
                    .await;
                }
                _ => {}
            }
            app.handle_event(&event);
        }
        Wake::AgentDone(done) => {
            app.running = false;
            driver.current_cancel = None;
            match done {
                DriverDone::Prompt(Ok(())) | DriverDone::Compact(Ok(Some(_))) => {}
                DriverDone::Prompt(Err(error)) => {
                    app.notice = Some(format!("agent loop 失败：{error}"));
                }
                // 压缩成功经 CompactionEnd 事件渲染与落库，这里无需重复处理
                DriverDone::Compact(Ok(None)) => {
                    app.notice = Some("上下文很短，没有可压缩的内容。".to_string());
                }
                DriverDone::Compact(Err(error)) => {
                    app.notice = Some(format!("压缩失败，上下文保持不变：{error}"));
                }
            }
        }
        Wake::Tick => app.tick(),
        Wake::Redraw => {}
        Wake::Closed => return true,
    }
    false
}

/// 等待下一个唤醒源：按键 / 鼠标 / agent 事件 / 本轮完成 / spinner 帧。
async fn next_wake(
    running: bool,
    term_events: &mut EventStream,
    spinner_ticker: &mut tokio::time::Interval,
    events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    done_rx: &mut mpsc::UnboundedReceiver<DriverDone>,
) -> Wake {
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
            Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                MouseEventKind::ScrollUp => Wake::ScrollUp,
                MouseEventKind::ScrollDown => Wake::ScrollDown,
                _ => Wake::Redraw,
            },
            Some(Ok(_)) => Wake::Redraw,
            Some(Err(_)) | None => Wake::Closed,
        },
        maybe_event = events.recv() => maybe_event.map_or(Wake::Closed, Wake::AgentEvent),
        maybe_done = done_rx.recv() => maybe_done.map_or(Wake::Closed, Wake::AgentDone),
    }
}

/// 键位处理（最小集，见 ADR-0002）。
async fn handle_key(
    app: &mut App,
    current_cancel: &mut Option<CancellationToken>,
    job_tx: &mpsc::UnboundedSender<DriverJob>,
    session: &mut Option<(SessionStore, String)>,
    cwd: &std::path::Path,
    skill_resolver: &SkillResolver,
    key: KeyEvent,
) {
    let cancel_running = |cancel: &Option<CancellationToken>| {
        if let Some(token) = cancel {
            token.cancel();
        }
    };
    // `/resume` 选择器打开时接管键位（slash 命令仅在空闲时可提交，
    // 此时 agent 必空闲，无运行可取消）
    if app.resume_picker().is_some() {
        handle_resume_key(app, job_tx, session, key).await;
        return;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) => {
            if app.running {
                cancel_running(current_cancel);
            } else {
                app.should_quit = true;
            }
        }
        (KeyCode::Esc, _) => {
            // 补全弹层可见时优先关闭弹层，否则取消当前运行
            if !app.dismiss_completion() && app.running {
                cancel_running(current_cancel);
            }
        }
        (KeyCode::Tab, _) => app.tab_complete(),
        // 换行必须在提交之前匹配；依赖 kitty 键盘增强协议区分两者
        (KeyCode::Enter, KeyModifiers::SHIFT) => app.insert_newline(),
        (KeyCode::Enter, _) => {
            if app.running {
                app.notice = Some("运行中，等待结束后再发送".to_string());
            } else if app.accept_completion_on_enter() {
                // 已填入补全候选；再次 Enter 提交
            } else if let Some(text) = app.take_input() {
                match app::parse_slash(&text) {
                    SlashParse::NotCommand => {
                        let token = CancellationToken::new();
                        *current_cancel = Some(token.clone());
                        // AgentStart 事件也会置位；先置避免提交空窗期重复提交
                        app.running = true;
                        app.notice = None;
                        let _ = job_tx.send(DriverJob::Prompt(text, token));
                    }
                    SlashParse::Known(action) => {
                        handle_slash(
                            app,
                            job_tx,
                            current_cancel,
                            session,
                            cwd,
                            skill_resolver,
                            action,
                        )
                        .await;
                    }
                    SlashParse::InvalidUsage(usage) => {
                        app.notice = Some(format!("参数形式不对，用法：{usage}"));
                    }
                    SlashParse::Unknown(name) => {
                        app.notice = Some(format!("未知命令 /{name}，输入 /help 查看可用命令"));
                    }
                }
            }
        }
        (KeyCode::Backspace, _) => app.backspace(),
        (KeyCode::Left, _) => app.cursor_left(),
        (KeyCode::Right, _) => app.cursor_right(),
        (KeyCode::Home, _) => app.cursor_home(),
        (KeyCode::End, _) => app.cursor_end(),
        // 补全弹层可见时 ↑/↓ 移动选中项，否则滚动聊天区
        (KeyCode::Up, _) => {
            if app.completion().is_some() {
                app.completion_select(-1);
            } else {
                app.scroll_up(1);
            }
        }
        (KeyCode::Down, _) => {
            if app.completion().is_some() {
                app.completion_select(1);
            } else {
                app.scroll_down(1);
            }
        }
        (KeyCode::PageUp, _) => app.scroll_up(ui::page_scroll()),
        (KeyCode::PageDown, _) => app.scroll_down(ui::page_scroll()),
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => app.insert_char(c),
        _ => {}
    }
}

/// 执行已知 slash 命令。
async fn handle_slash(
    app: &mut App,
    job_tx: &mpsc::UnboundedSender<DriverJob>,
    current_cancel: &mut Option<CancellationToken>,
    session: &mut Option<(SessionStore, String)>,
    cwd: &std::path::Path,
    skill_resolver: &SkillResolver,
    action: SlashAction,
) {
    match action {
        SlashAction::Help => app.push_system(app::help_text()),
        SlashAction::Quit => app.should_quit = true,
        SlashAction::Compact(instructions) => {
            // 压缩是一次 LLM 调用：按 mini-run 处理，Esc 可取消
            let token = CancellationToken::new();
            *current_cancel = Some(token.clone());
            app.running = true;
            app.notice = None;
            let _ = job_tx.send(DriverJob::Compact(instructions, token));
        }
        SlashAction::Resume => match session_store(session.as_ref()).await {
            Err(error) => app.notice = Some(format!("{error:#}")),
            Ok(store) => match store.list_sessions().await {
                Err(error) => app.notice = Some(format!("列出 session 失败：{error}")),
                Ok(sessions) if sessions.is_empty() => {
                    app.push_system("没有历史 session。");
                }
                Ok(sessions) => {
                    let rows = sessions
                        .iter()
                        .map(|summary| app::ResumeRow {
                            id: summary.id.clone(),
                            text: crate::sessions::row_text(summary),
                        })
                        .collect();
                    app.open_resume_picker(rows);
                }
            },
        },
        SlashAction::Skill(None) => {
            // 列出时顺带刷新补全快照：会话期间新增的 skill 也能被 Tab 补全
            let catalog = skill_resolver.catalog();
            app.set_available_skills(
                catalog
                    .iter()
                    .map(|skill| SkillEntry {
                        name: skill.name.clone(),
                        description: skill.document.description.clone(),
                    })
                    .collect(),
            );
            app.push_system(app::skill_list_text(&catalog));
        }
        SlashAction::Skill(Some(name)) => match skill_resolver.activate(&name) {
            Ok(skill) => {
                // 注入消息经事件管线回流：聊天区压缩展示 + session 落库自动生效
                let _ = job_tx.send(DriverJob::Inject(app::skill_load_message(&skill)));
            }
            Err(error) => {
                app.notice = Some(format!("载入 skill {name:?} 失败：{error}"));
            }
        },
        SlashAction::New => {
            // driver 串行处理任务；slash 命令仅在空闲时可提交，无需排队等待
            let _ = job_tx.send(DriverJob::Clear);
            app.clear_items();
            app.push_system("已开启新对话，上下文已清空。");
            if let Some((store, id)) = session {
                match store.create_session(cwd).await {
                    Ok(new_id) => {
                        id.clone_from(&new_id);
                        app.session_id = Some(new_id);
                    }
                    Err(error) => {
                        app.notice =
                            Some(format!("创建新 session 失败，续写当前 session：{error}"));
                    }
                }
            }
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

/// `/resume` 选择器打开时的键位：↑/↓/j/k 移动，Enter 恢复选中 session，
/// Esc/q 取消；Ctrl+C/D 保持全局退出，其余输入忽略（避免污染输入缓冲）。
async fn handle_resume_key(
    app: &mut App,
    job_tx: &mpsc::UnboundedSender<DriverJob>,
    session: &mut Option<(SessionStore, String)>,
    key: KeyEvent,
) {
    match (key.code, key.modifiers) {
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => app.resume_select(-1),
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => app.resume_select(1),
        (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => app.close_resume_picker(),
        (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) => app.should_quit = true,
        (KeyCode::Enter, _) => {
            if let Some(id) = app.take_resume_selection() {
                resume_session(app, job_tx, session, id).await;
            }
        }
        _ => {}
    }
}

/// 恢复选中 session：加载历史 → 替换 agent 上下文与聊天区 → 切换落库目标。
async fn resume_session(
    app: &mut App,
    job_tx: &mpsc::UnboundedSender<DriverJob>,
    session: &mut Option<(SessionStore, String)>,
    id: String,
) {
    let loaded = async {
        let store = session_store(session.as_ref()).await?;
        let messages = store
            .load_messages(&id)
            .await
            .with_context(|| format!("加载 session {id} 失败"))?;
        Ok::<_, anyhow::Error>((store, messages))
    }
    .await;
    match loaded {
        Err(error) => app.notice = Some(format!("恢复 session 失败：{error:#}")),
        Ok((store, messages)) => {
            // driver 串行处理任务：紧随其后的 prompt 一定排在 Restore 之后，
            // 不会出现「新 prompt 跑在旧上下文」的交错
            let _ = job_tx.send(DriverJob::Restore(messages.clone()));
            app.clear_items();
            app.load_history(&messages);
            match session {
                Some((_, current)) => current.clone_from(&id),
                current @ None => *current = Some((store, id.clone())),
            }
            app.session_id = Some(id.clone());
            app.push_system(format!(
                "已恢复 session {}（{} 条消息），后续对话续写该 session。",
                crate::sessions::short_id(&id),
                messages.len()
            ));
        }
    }
}

/// `MessageEnd` 定稿点落库；失败仅提示不中断（store 非权威源）。
async fn persist(session: Option<&(SessionStore, String)>, message: &Message, app: &mut App) {
    if let Some((store, session_id)) = session
        && let Err(error) = store.append_message(session_id, None, message).await
    {
        app.notice = Some(format!("session 落库失败：{error}"));
    }
}

/// `CompactionEnd` 落库压缩条目；失败仅提示不中断（与消息落库同一策略）。
async fn persist_compaction(
    session: Option<&(SessionStore, String)>,
    summary: &str,
    tokens_before: u64,
    kept_count: usize,
    app: &mut App,
) {
    let Some((store, session_id)) = session else {
        return;
    };
    let record = CompactionRecord {
        summary: summary.to_string(),
        kept_count: kept_count as u64,
        tokens_before,
    };
    if let Err(error) = store.append_compaction(session_id, &record).await {
        app.notice = Some(format!("compaction 落库失败：{error}"));
    }
}

/// 终端状态守卫：进入 raw mode + alternate screen + 鼠标捕获；
/// Drop（含 panic 路径经 hook）时恢复。
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
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
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        default_hook(info);
    }));
}
