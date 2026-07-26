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
mod ui;

use std::io;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt as _;
use nomic_ai::Message;
use nomic_core::{Agent, AgentConfig, AgentEvent, ExecutionMode, NoopHooks};
use nomic_session::SessionStore;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use app::App;

use crate::{Cli, bootstrap};

/// 提交给 agent driver 的一次运行：prompt 文本 + 本轮取消令牌。
type PromptJob = (String, CancellationToken);

/// 运行交互 TUI。
pub async fn run(cli: &Cli) -> Result<()> {
    let boot = bootstrap::bootstrap(cli).await?;

    let mut app = App::new(
        boot.model.name.clone(),
        boot.session.as_ref().map(|(_, id)| id.clone()),
    );
    app.load_history(&boot.history);

    let (agent, mut events) = Agent::with_messages(
        AgentConfig {
            model: boot.model,
            provider: boot.provider,
            stream_options: boot.stream_options,
            hooks: Arc::new(NoopHooks),
            tool_execution: ExecutionMode::Parallel,
        },
        nomic_tools::default_tools(),
        boot.system_prompt,
        boot.history,
    );

    let _guard = TerminalGuard::enter().context("初始化终端失败")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("创建终端后端失败")?;

    // agent driver：持有 Agent，串行执行 prompt，完成后回传结果
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<PromptJob>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Result<(), String>>();
    tokio::spawn(async move {
        let mut agent = agent;
        while let Some((text, cancel)) = job_rx.recv().await {
            let result = agent
                .prompt(&text, cancel)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
            if done_tx.send(result).is_err() {
                return;
            }
        }
    });

    let mut term_events = EventStream::new();
    let mut current_cancel: Option<CancellationToken> = None;
    loop {
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .context("绘制失败")?;
        tokio::select! {
            maybe_event = term_events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                    handle_key(&mut app, &mut current_cancel, &job_tx, key);
                }
                Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                    MouseEventKind::ScrollUp => app.scroll_up(3),
                    MouseEventKind::ScrollDown => app.scroll_down(3),
                    _ => {}
                },
                // resize 等：下一轮循环自然重绘
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
            maybe_event = events.recv() => {
                let Some(event) = maybe_event else { break };
                if let AgentEvent::MessageEnd(message) = &event {
                    persist(boot.session.as_ref(), message, &mut app).await;
                }
                app.handle_event(&event);
            }
            maybe_done = done_rx.recv() => {
                let Some(done) = maybe_done else { break };
                app.running = false;
                current_cancel = None;
                if let Err(error) = done {
                    app.notice = Some(format!("agent loop 失败：{error}"));
                }
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// 键位处理（最小集，见 ADR-0002）。
fn handle_key(
    app: &mut App,
    current_cancel: &mut Option<CancellationToken>,
    job_tx: &mpsc::UnboundedSender<PromptJob>,
    key: KeyEvent,
) {
    let cancel_running = |cancel: &Option<CancellationToken>| {
        if let Some(token) = cancel {
            token.cancel();
        }
    };
    match (key.code, key.modifiers) {
        (KeyCode::Char('c' | 'd'), KeyModifiers::CONTROL) => {
            if app.running {
                cancel_running(current_cancel);
            } else {
                app.should_quit = true;
            }
        }
        (KeyCode::Esc, _) => {
            if app.running {
                cancel_running(current_cancel);
            }
        }
        (KeyCode::Enter, _) => {
            if app.running {
                app.notice = Some("运行中，等待结束后再发送".to_string());
            } else if let Some(text) = app.take_input() {
                let token = CancellationToken::new();
                *current_cancel = Some(token.clone());
                // AgentStart 事件也会置位；先置避免提交空窗期重复提交
                app.running = true;
                app.notice = None;
                let _ = job_tx.send((text, token));
            }
        }
        (KeyCode::Backspace, _) => app.backspace(),
        (KeyCode::Left, _) => app.cursor_left(),
        (KeyCode::Right, _) => app.cursor_right(),
        (KeyCode::Home, _) => app.cursor_home(),
        (KeyCode::End, _) => app.cursor_end(),
        (KeyCode::Up, _) => app.scroll_up(1),
        (KeyCode::Down, _) => app.scroll_down(1),
        (KeyCode::PageUp, _) => app.scroll_up(ui::page_scroll()),
        (KeyCode::PageDown, _) => app.scroll_down(ui::page_scroll()),
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => app.insert_char(c),
        _ => {}
    }
}

/// `MessageEnd` 定稿点落库；失败仅提示不中断（store 非权威源）。
async fn persist(session: Option<&(SessionStore, String)>, message: &Message, app: &mut App) {
    if let Some((store, session_id)) = session {
        if let Err(error) = store.append_message(session_id, None, message).await {
            app.notice = Some(format!("session 落库失败：{error}"));
        }
    }
}

/// 终端状态守卫：进入 raw mode + alternate screen + 鼠标捕获；
/// Drop（含 panic 路径经 hook）时恢复。
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        install_panic_hook();
        Ok(Self)
    }

    fn restore() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
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
