//! 历史 session CLI：`nomic sessions list`，以及顶层 `nomic resume` 交互选择器。

use std::io::{self, IsTerminal as _, Write};

use anyhow::{Context as _, Result, bail};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::{
    cursor, execute, queue,
    style::{self, Attribute},
    terminal::{self, ClearType},
};
use nomic_session::{SessionStore, SessionSummary};
use time::macros::format_description;

use crate::Cli;

/// 列出全部 session：id、最后更新时间、消息数与启动目录。
pub async fn list() -> Result<()> {
    let store = SessionStore::open_default()
        .await
        .context("打开 session 库失败")?;
    let sessions = store.list_sessions().await.context("列出 session 失败")?;
    if sessions.is_empty() {
        println!("没有历史 session。");
        return Ok(());
    }
    for summary in sessions {
        println!(
            "{}  {}  {:>4} 条消息  {}",
            summary.id,
            format_time(summary.last_message_at),
            summary.message_count,
            summary.cwd.display()
        );
    }
    Ok(())
}

/// 交互选择历史 session 并恢复：确认后按原运行模式（TUI/print）载入该 session。
///
/// 选择器需要交互终端；非 TTY 场景（管道、脚本）报错并提示用 `--session <ID>`。
/// 用户取消（Esc/q/Ctrl-C）时静默退出，不进入对话。
pub async fn resume(cli: &Cli) -> Result<()> {
    let store = SessionStore::open_default()
        .await
        .context("打开 session 库失败")?;
    let sessions = store.list_sessions().await.context("列出 session 失败")?;
    if sessions.is_empty() {
        println!("没有历史 session。");
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "resume 需要交互终端选择 session；\
             非交互场景请用 --session <ID> 指定（`nomic sessions list` 查看全部）"
        );
    }
    let Some(id) = pick_session(&sessions)? else {
        return Ok(());
    };
    crate::dispatch(&resume_cli(cli, id)).await
}

/// 选中 session 后的常规 CLI：仅清除子命令与 `--continue`，再复用 `--session` 恢复路径。
fn resume_cli(cli: &Cli, id: String) -> Cli {
    let mut cli = cli.clone();
    cli.command = None;
    cli.continue_session = false;
    cli.session = Some(id);
    cli
}

/// 选择器纯状态：当前选中行与滚动窗口起点（脱离终端可测）。
#[derive(Debug, Default, PartialEq, Eq)]
struct Picker {
    selected: usize,
    offset: usize,
}

impl Picker {
    /// 上移一行；选中行滚出窗口上沿时同步收缩窗口。
    fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.offset = self.offset.min(self.selected);
    }

    /// 下移一行（到底停止）；选中行滚出窗口下沿时同步下推窗口。
    fn down(&mut self, len: usize, capacity: usize) {
        if len == 0 {
            self.selected = 0;
            self.offset = 0;
            return;
        }
        self.selected = self.selected.saturating_add(1).min(len - 1);
        let capacity = capacity.max(1).min(len);
        if self.selected >= self.offset.saturating_add(capacity) {
            self.offset = self.selected.saturating_add(1).saturating_sub(capacity);
        }
    }

    /// 实际绘制用的窗口起点：钳制在合法范围，并兜底保证选中行可见
    /// （终端突然变矮时状态里的 offset 可能失效）。
    fn window(&self, len: usize, capacity: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let capacity = capacity.min(len).max(1);
        let selected = self.selected.min(len - 1);
        self.offset
            .min(selected)
            .max(selected.saturating_sub(capacity - 1))
            .min(len - capacity)
    }
}

/// 键盘输入对应的 picker 行为（脱离终端事件循环可测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerAction {
    Up,
    Down,
    Confirm,
    Cancel,
    Ignore,
}

/// ↑/↓ 或 j/k 移动，Enter 确认，Esc/q/Ctrl-C 取消；release 事件忽略。
fn key_action(key: &KeyEvent) -> PickerAction {
    if key.kind == KeyEventKind::Release {
        return PickerAction::Ignore;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => PickerAction::Up,
        KeyCode::Down | KeyCode::Char('j') => PickerAction::Down,
        KeyCode::Enter => PickerAction::Confirm,
        KeyCode::Esc | KeyCode::Char('q') => PickerAction::Cancel,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => PickerAction::Cancel,
        _ => PickerAction::Ignore,
    }
}

/// raw mode 与光标的 RAII 恢复：任何成功、取消或错误路径离开作用域都会执行。
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

/// 终端选择器：返回选中 session 的 id；取消返回 `None`。
fn pick_session(sessions: &[SessionSummary]) -> Result<Option<String>> {
    terminal::enable_raw_mode().context("初始化选择器失败")?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    pick_loop(&mut stdout, sessions)
}

/// 选择器主循环（raw mode 内）：重绘 → 读键 → 更新状态，直到确认或取消。
/// 返回前总是清理选择器区域，即使绘制或读键中途失败。
fn pick_loop(stdout: &mut impl Write, sessions: &[SessionSummary]) -> Result<Option<String>> {
    if sessions.is_empty() {
        return Ok(None);
    }
    execute!(stdout, cursor::Hide)?;
    let mut printed = 0_u16;
    let result = pick_events(stdout, sessions, &mut printed);
    let clear = clear_picker(stdout, printed);
    match (result, clear) {
        (Ok(selection), Ok(())) => Ok(selection),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

/// 事件循环内核；清理由调用方统一负责。
fn pick_events(
    stdout: &mut impl Write,
    sessions: &[SessionSummary],
    printed: &mut u16,
) -> Result<Option<String>> {
    let mut picker = Picker::default();
    loop {
        // 回到上一帧块首并清除旧内容后重绘
        if *printed > 0 {
            queue!(
                stdout,
                cursor::MoveToPreviousLine(*printed),
                terminal::Clear(ClearType::FromCursorDown)
            )?;
        }
        *printed = draw_picker(stdout, sessions, &picker)?;
        stdout.flush()?;
        let capacity = row_capacity();
        if let Event::Key(key) = crossterm::event::read()? {
            match key_action(&key) {
                PickerAction::Up => picker.up(),
                PickerAction::Down => picker.down(sessions.len(), capacity),
                PickerAction::Confirm => {
                    return Ok(Some(sessions[picker.selected].id.clone()));
                }
                PickerAction::Cancel => return Ok(None),
                PickerAction::Ignore => {}
            }
        }
    }
}

/// 终端高度；读取失败时使用常见默认值。
fn terminal_height() -> u16 {
    terminal::size().map_or(24, |(_, height)| height)
}

/// 帧布局：是否显示表头，以及 session 行容量。
/// 高度 1 时只绘制选中行；高度 0 时不绘制，避免越界写屏。
fn frame_layout(height: u16) -> (bool, usize) {
    match height {
        0 => (false, 0),
        1 => (false, 1),
        _ => (true, usize::from(height) - 1),
    }
}

/// 可见行容量。
fn row_capacity() -> usize {
    frame_layout(terminal_height()).1
}

/// 绘制一帧（表头 + 可见窗口内的行），返回绘制的行数。
fn draw_picker(
    stdout: &mut impl Write,
    sessions: &[SessionSummary],
    picker: &Picker,
) -> io::Result<u16> {
    draw_picker_with_height(stdout, sessions, picker, terminal_height())
}

/// 按指定终端高度绘制一帧（脱离真实终端尺寸可测）。
fn draw_picker_with_height(
    stdout: &mut impl Write,
    sessions: &[SessionSummary],
    picker: &Picker,
    height: u16,
) -> io::Result<u16> {
    let (show_header, capacity) = frame_layout(height);
    let mut lines = 0_u16;
    if show_header {
        queue!(
            stdout,
            style::Print("选择要恢复的 session（↑/↓ 或 j/k 移动，Enter 确认，Esc/q 取消）\r\n")
        )?;
        lines += 1;
    }
    if capacity == 0 {
        return Ok(lines);
    }
    let offset = picker.window(sessions.len(), capacity);
    for (index, summary) in sessions.iter().enumerate().skip(offset).take(capacity) {
        let selected = index == picker.selected;
        if selected {
            queue!(stdout, style::SetAttribute(Attribute::Reverse))?;
        }
        queue!(
            stdout,
            style::Print(format!(
                "{}{}",
                if selected { "› " } else { "  " },
                row_text(summary)
            )),
            style::SetAttribute(Attribute::Reset),
            style::Print("\r\n")
        )?;
        lines += 1;
    }
    Ok(lines)
}

/// 选择结束后清除选择器区域，把干净的终端交还给后续 TUI/print 输出。
fn clear_picker(stdout: &mut impl Write, printed: u16) -> io::Result<()> {
    execute!(
        stdout,
        cursor::MoveToPreviousLine(printed),
        terminal::Clear(ClearType::FromCursorDown)
    )
}

/// 一行的展示文本：短 id、最后更新时间、消息数与启动目录。
/// CLI 选择器与 TUI `/resume` 弹层共用。
pub fn row_text(summary: &SessionSummary) -> String {
    format!(
        "{}  {}  {:>4} 条消息  {}",
        short_id(&summary.id),
        format_time(summary.last_message_at),
        summary.message_count,
        summary.cwd.display()
    )
}

/// UUID 取前 8 位用于紧凑展示（选择器内无需完整 id）。
pub fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Unix 毫秒时间戳 → `YYYY-MM-DD HH:MM`（本地时区，失败退回 UTC；无值显示 `-`）。
pub fn format_time(timestamp_ms: Option<u64>) -> String {
    const FORMAT: &[time::format_description::FormatItem<'static>] =
        format_description!("[year]-[month]-[day] [hour]:[minute]");
    let Some(ms) = timestamp_ms else {
        return "-".to_string();
    };
    let Ok(secs) = i64::try_from(ms / 1000) else {
        return "-".to_string();
    };
    let Ok(utc) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return "-".to_string();
    };
    let local = time::UtcOffset::current_local_offset().map_or(utc, |offset| utc.to_offset(offset));
    local.format(FORMAT).unwrap_or_else(|_| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_timestamp_shows_dash() {
        assert_eq!(format_time(None), "-");
    }

    #[test]
    fn formats_valid_timestamp() {
        // 2026-07-26T14:48:00Z 附近；本地时区只影响小时位，格式形状不变
        let text = format_time(Some(1_785_000_000_000));
        assert_eq!(text.len(), 16, "应为 YYYY-MM-DD HH:MM，实际：{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[13..14], ":");
    }

    #[test]
    fn out_of_range_timestamp_shows_dash() {
        assert_eq!(format_time(Some(u64::MAX)), "-");
    }

    // ── resume 选择器：纯状态与展示 ──────────────────────────────────────────

    #[test]
    fn picker_up_clamps_at_top() {
        let mut picker = Picker::default();
        picker.up();
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.offset, 0);
    }

    #[test]
    fn picker_down_clamps_at_bottom() {
        let mut picker = Picker::default();
        for _ in 0..10 {
            picker.down(3, 10);
        }
        assert_eq!(picker.selected, 2);
    }

    #[test]
    fn picker_down_scrolls_window_to_keep_selection_visible() {
        let mut picker = Picker::default();
        for _ in 0..5 {
            picker.down(10, 3);
        }
        assert_eq!(picker.selected, 5);
        assert_eq!(picker.offset, 3, "选中行应贴在窗口下沿");
        assert_eq!(picker.window(10, 3), 3);
    }

    #[test]
    fn picker_up_scrolls_window_back() {
        let mut picker = Picker {
            selected: 5,
            offset: 3,
        };
        for _ in 0..4 {
            picker.up();
        }
        assert_eq!(picker.selected, 1);
        assert_eq!(picker.offset, 1, "选中行应贴在窗口上沿");
    }

    #[test]
    fn picker_window_keeps_selection_visible_after_shrink() {
        // 终端变矮导致容量缩小：旧 offset 让选中行跑出窗口时兜底修正
        let picker = Picker {
            selected: 9,
            offset: 4,
        };
        assert_eq!(picker.window(10, 3), 7);
    }

    #[test]
    fn picker_handles_zero_capacity_and_empty_list() {
        let mut picker = Picker {
            selected: 5,
            offset: 3,
        };
        assert_eq!(picker.window(0, 0), 0);
        picker.down(0, 0);
        assert_eq!(picker, Picker::default());

        picker.down(3, 0);
        assert_eq!(picker.selected, 1);
        assert_eq!(picker.window(3, 0), 1, "容量 0 按至少一行保证选中行可见");
    }

    #[test]
    fn picker_with_single_session_stays_on_first_row() {
        let mut picker = Picker::default();
        picker.down(1, 1);
        picker.up();
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.window(1, 1), 0);
    }

    #[test]
    fn key_action_maps_navigation_confirm_and_cancel() {
        assert_eq!(
            key_action(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            PickerAction::Up
        );
        assert_eq!(
            key_action(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            PickerAction::Up
        );
        assert_eq!(
            key_action(&KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            PickerAction::Down
        );
        assert_eq!(
            key_action(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            PickerAction::Down
        );
        assert_eq!(
            key_action(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            PickerAction::Confirm
        );
        for key in [
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            assert_eq!(key_action(&key), PickerAction::Cancel);
        }
    }

    #[test]
    fn key_action_ignores_release_and_unmapped_keys() {
        assert_eq!(
            key_action(&KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )),
            PickerAction::Ignore
        );
        assert_eq!(
            key_action(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            PickerAction::Ignore
        );
    }

    #[test]
    fn frame_layout_avoids_overflow_on_tiny_terminals() {
        assert_eq!(frame_layout(0), (false, 0));
        assert_eq!(frame_layout(1), (false, 1));
        assert_eq!(frame_layout(2), (true, 1));
    }

    #[test]
    fn draw_picker_respects_tiny_terminal_height() {
        let sessions = vec![picker_summary("01999999-aaaa-bbbb-cccc")];
        let picker = Picker::default();

        let mut output = Vec::new();
        let lines = draw_picker_with_height(&mut output, &sessions, &picker, 0).unwrap();
        assert_eq!(lines, 0);
        assert!(output.is_empty());

        let mut output = Vec::new();
        let lines = draw_picker_with_height(&mut output, &sessions, &picker, 1).unwrap();
        assert_eq!(lines, 1);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("01999999"), "{output:?}");
        assert!(!output.contains("选择要恢复的 session"), "{output:?}");

        let mut output = Vec::new();
        let lines = draw_picker_with_height(&mut output, &sessions, &picker, 2).unwrap();
        assert_eq!(lines, 2);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("选择要恢复的 session"), "{output:?}");
        assert!(output.contains("01999999"), "{output:?}");
    }

    #[test]
    fn resume_cli_reuses_session_path_without_touching_other_options() {
        use clap::Parser as _;

        let cli = Cli::try_parse_from([
            "nomic",
            "--model",
            "model-x",
            "-p",
            "hi",
            "--continue",
            "resume",
        ])
        .expect("resume 参数应可解析");
        let selected = resume_cli(&cli, "selected-id".to_string());

        assert!(selected.command.is_none());
        assert!(!selected.continue_session);
        assert_eq!(selected.session.as_deref(), Some("selected-id"));
        assert_eq!(selected.model.as_deref(), Some("model-x"));
        assert_eq!(selected.print.as_deref(), Some("hi"));
    }

    fn picker_summary(id: &str) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            cwd: std::path::PathBuf::from("/tmp/project"),
            first_message_at: Some(1_785_000_000_000),
            last_message_at: Some(1_785_000_000_000),
            message_count: 1,
        }
    }

    #[test]
    fn short_id_truncates_to_8_chars() {
        assert_eq!(short_id("01999999-aaaa-bbbb-cccc"), "01999999");
        assert_eq!(short_id("short"), "short");
    }
}
