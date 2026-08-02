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
use nomic_ai::Message;
use nomic_core::{Agent, AgentEvent, Compaction};
use nomic_session::{CompactionRecord, SessionStore};
use nomic_skills::SkillResolver;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use app::{App, Effect, Key, ResumeRow, SkillEntry};

use crate::{Cli, bootstrap};

/// 提交给 agent driver 的任务。
enum DriverJob {
    /// 运行一轮 prompt（附图片附件与本轮取消令牌）
    Prompt(String, Vec<nomic_ai::ImageContent>, CancellationToken),
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

    let (agent, mut events) = Agent::builder()
        .model(boot.model)
        .provider(boot.provider)
        .system_prompt(boot.system_prompt)
        .tools(nomic_tools::default_tools_with_skills(boot.skill_resolver))
        .messages(boot.history)
        .stream_options(boot.stream_options)
        .compaction(boot.compaction)
        .build();

    let _guard = TerminalGuard::enter().context("初始化终端失败")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("创建终端后端失败")?;

    // agent driver：持有 Agent，串行执行 prompt，完成后回传结果
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<DriverJob>();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<DriverDone>();
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
        task: Some(driver_task),
        alive: true,
        session: boot.session,
        cwd: std::env::current_dir().context("get cwd")?,
        skill_resolver,
    };
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

/// 事件循环持有的驱动端资源。
struct Driver {
    job_tx: mpsc::UnboundedSender<DriverJob>,
    current_cancel: Option<CancellationToken>,
    /// driver 任务的 JoinHandle：任务 panic 时取出详情转为 TUI 内错误提示
    task: Option<tokio::task::JoinHandle<()>>,
    /// driver 是否存活；退出后其 channel 已关闭，事件循环跳过对应分支
    alive: bool,
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
            driver.current_cancel = None;
            let notice = match done {
                DriverDone::Prompt(Ok(())) | DriverDone::Compact(Ok(Some(_))) => None,
                DriverDone::Prompt(Err(error)) => Some(format!("agent loop 失败：{error}")),
                // 压缩成功经 CompactionEnd 事件渲染与落库，这里无需重复处理
                DriverDone::Compact(Ok(None)) => Some("上下文很短，没有可压缩的内容。".to_string()),
                DriverDone::Compact(Err(error)) => {
                    Some(format!("压缩失败，上下文保持不变：{error}"))
                }
            };
            app.finish_run(notice);
        }
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
        Effect::Cancel => {
            if let Some(token) = &driver.current_cancel {
                token.cancel();
            }
        }
        Effect::ListSessions => list_sessions(app, driver).await,
        Effect::Resume(id) => {
            resume_session(app, &driver.job_tx, &mut driver.session, id).await;
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
                    .map(|summary| ResumeRow {
                        id: summary.id.clone(),
                        text: crate::sessions::row_text(summary),
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
        Err(error) => app.warn(format!("恢复 session 失败：{error:#}")),
        Ok((store, messages)) => {
            // driver 串行处理任务：紧随其后的 prompt 一定排在 Restore 之后，
            // 不会出现「新 prompt 跑在旧上下文」的交错
            let _ = job_tx.send(DriverJob::Restore(messages.clone()));
            app.restore_conversation(&messages, id.clone());
            match session {
                Some((_, current)) => current.clone_from(&id),
                current @ None => *current = Some((store, id.clone())),
            }
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
        app.warn(format!("session 落库失败：{error}"));
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
        app.warn(format!("compaction 落库失败：{error}"));
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

    use super::{panic_payload_text, paste_image_path, percent_decode};

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
