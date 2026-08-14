//! 终端生命周期：TUI 终端态进入/离开（raw mode + alternate screen +
//! 鼠标捕获 + bracketed paste + kitty 键盘增强）、panic 恢复 hook、
//! INSERT `Ctrl+G` 外部编辑器（ADR-0017）接线与光标形状切换。

use std::io;

use anyhow::{Context as _, Result};
use crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};

use super::TuiTerminal;
use super::app::{App, Mode};

/// 光标是否用实心块：NORMAL/HELP 与 QUEUE 导航子状态为实心块
///（不可键入文本的浏览态）；COMMAND 是键入态，用竖条。
pub(super) const fn block_cursor(app: &App) -> bool {
    match app.mode() {
        Mode::Normal | Mode::Help => true,
        Mode::Queue => !app.queue().is_editing(),
        Mode::Insert | Mode::Command | Mode::Picker => false,
    }
}

/// 应用光标形状：实心块（浏览态）/ 竖条（可键入态）。
pub(super) fn set_cursor_style(block: bool) {
    let style = if block {
        SetCursorStyle::SteadyBlock
    } else {
        SetCursorStyle::SteadyBar
    };
    let _ = execute!(io::stdout(), style);
}
/// INSERT `Ctrl+G`：挂起 TUI，用外部编辑器编辑当前输入缓冲，退出后
/// 恢复终端并把结果写回（写回语义见 [`App::apply_editor_result`]）。
///
/// 编辑器运行期间事件循环挂起是本意的同步语义：tty 已交给编辑器，
/// TUI 不应重绘；crossterm 的 EventStream 后台线程只 poll 就绪不读
/// 字节（0.29 起 read 发生在消费侧 poll_next），不轮询就不会与编辑器
/// 争抢 stdin，编辑器里的按键不会漏回 TUI。agent 运行不受影响
///（driver 是独立任务），期间到的事件在 channel 里积压，恢复后照常处理。
pub(super) async fn edit_input_in_editor(app: &mut App, terminal: &mut TuiTerminal) {
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
pub(super) struct TerminalGuard;

impl TerminalGuard {
    pub(super) fn enter() -> io::Result<Self> {
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
