//! TUI 渲染：从 [`App`] 状态构建 ratatui 画面（聊天区 + 输入框 + 状态栏）。

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Position, Rect},
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph},
};
use unicode_width::UnicodeWidthChar;

use super::{
    app::{App, Block, ChatItem, Completion, CompletionCandidate, ToolStatus},
    theme,
};

/// 单页滚动的行数。
const PAGE_SCROLL: u16 = 10;

/// 绘制整帧。
pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(frame.area());
    // Paragraph 不清理文本以外的单元格；先 Clear 避免长行残留
    frame.render_widget(Clear, frame.area());
    draw_chat(frame, app, chunks[0]);
    draw_input(frame, app, chunks[1]);
    draw_status(frame, app, chunks[2]);
    if let Some(completion) = app.completion() {
        draw_completion(frame, completion, chunks[1]);
    }
}

/// 聊天区：历史条目 + 流式累积，软换行，`scroll` 从底部向上计。
fn draw_chat(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    if app.items.is_empty() {
        app.scroll_max = 0;
        draw_welcome(frame, app, area);
        return;
    }
    let spinner = app.spinner();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for item in &app.items {
        match item {
            ChatItem::User(text) => {
                // 左侧 accent 竖条把整条用户消息包成视觉块，多轮对话里可扫读
                let mut text_lines = text.lines().peekable();
                if text_lines.peek().is_none() {
                    lines.push(Line::from(Span::styled("▌", theme::user_marker())));
                }
                for line in text_lines {
                    lines.push(Line::from(vec![
                        Span::styled("▌ ", theme::user_marker()),
                        Span::styled(line.to_string(), theme::user_text()),
                    ]));
                }
                lines.push(Line::default());
            }
            ChatItem::Assistant(assistant) => {
                for block in &assistant.blocks {
                    match block {
                        Block::Text(text) => {
                            lines.extend(text.lines().map(|line| Line::from(line.to_string())));
                        }
                        Block::Thinking(thinking) => {
                            lines.extend(thinking.lines().map(|line| {
                                Line::from(Span::styled(line.to_string(), theme::thinking()))
                            }));
                        }
                    }
                }
                if let Some(error) = &assistant.error {
                    lines.push(Line::from(Span::styled(format!("✗ {error}"), theme::err())));
                } else if !assistant.done {
                    // 流式指示：消息未定稿时提示仍在生成，避免长 thinking 看似卡死
                    lines.push(Line::from(vec![
                        Span::styled(format!("{spinner} "), theme::busy()),
                        Span::styled("生成中…", theme::dim()),
                    ]));
                }
                if !assistant.blocks.is_empty() || assistant.error.is_some() {
                    lines.push(Line::default());
                }
            }
            ChatItem::System(text) => {
                lines.extend(
                    text.lines()
                        .map(|line| Line::from(Span::styled(line.to_string(), theme::dim()))),
                );
                lines.push(Line::default());
            }
            ChatItem::Tool(tool) => {
                // 树形条目：状态色标记 + 加粗工具名 + 暗色 (参数)，结果行缩进对齐
                let (mark, mark_style, name_style) = match tool.status {
                    ToolStatus::Running => (spinner, theme::busy(), theme::bold()),
                    ToolStatus::Ok => ("⏺", theme::ok(), theme::bold()),
                    ToolStatus::Failed => ("⏺", theme::err(), theme::err_bold()),
                };
                let mut spans = vec![
                    Span::styled(format!("{mark} "), mark_style),
                    Span::styled(tool.name.clone(), name_style),
                ];
                if !tool.args.is_empty() {
                    spans.push(Span::styled(format!("({})", tool.args), theme::dim()));
                }
                lines.push(Line::from(spans));
                if let Some(detail) = &tool.detail {
                    let detail_style = if tool.status == ToolStatus::Failed {
                        theme::err()
                    } else {
                        theme::dim()
                    };
                    lines.push(Line::from(Span::styled(
                        format!("  ⎿ {detail}"),
                        detail_style,
                    )));
                }
            }
        }
    }
    if app.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "输入 prompt 开始对话。Enter 发送，Ctrl+C 退出。",
            theme::dim(),
        )));
    }

    // 自行折行（硬换行，CJK 友好），使行数精确可知、滚动偏移精确
    let lines = wrap_lines(&lines, area.width);
    let total = lines.len();
    let max_scroll = total.saturating_sub(usize::from(area.height));
    app.scroll = app
        .scroll
        .min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
    app.scroll_max = u16::try_from(max_scroll).unwrap_or(u16::MAX);
    let offset = max_scroll.saturating_sub(usize::from(app.scroll));
    let offset = u16::try_from(offset).unwrap_or(u16::MAX);
    let paragraph = Paragraph::new(lines).scroll((offset, 0));
    frame.render_widget(paragraph, area);
}

/// 空状态欢迎页：居中 logo + 键位速查。
fn draw_welcome(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            format!("▌ nomic v{}", env!("CARGO_PKG_VERSION")),
            theme::user_marker(),
        )),
        Line::from(Span::styled(
            format!("agent TUI · {}", app.model_name),
            theme::dim(),
        )),
        Line::default(),
        Line::from(Span::styled(
            "Enter 发送 · / 命令（Tab 补全，/help 查看全部）",
            theme::dim(),
        )),
        Line::from(Span::styled(
            "↑↓/PgUp/PgDn 滚动 · Esc 取消 · Ctrl+C 退出",
            theme::dim(),
        )),
    ];
    // 垂直居中（空间不足时贴顶），水平居中由 Paragraph 对齐负责
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let y = area.y + area.height.saturating_sub(height) / 2;
    let centered = Rect {
        x: area.x,
        y,
        width: area.width,
        height: height.min(area.height),
    };
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), centered);
}

/// 把逻辑行按显示宽度折成物理行（保留 span 样式）。
fn wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let max = usize::from(width).max(1);
    let mut out = Vec::new();
    for line in lines {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut row = 0usize;
        let mut buf = String::new();
        for span in &line.spans {
            for c in span.content.chars() {
                let char_width = UnicodeWidthChar::width(c).unwrap_or(0);
                if row + char_width > max {
                    spans.push(Span::styled(std::mem::take(&mut buf), span.style));
                    out.push(Line::from(std::mem::take(&mut spans)));
                    row = 0;
                }
                buf.push(c);
                row += char_width;
            }
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), span.style));
            }
        }
        out.push(Line::from(spans));
    }
    out
}

/// 输入框（单行）+ 光标定位。
fn draw_input(frame: &mut Frame<'_>, app: &App, area: Rect) {
    // 三态边框：运行中（黄 + spinner）/ 补全打开（accent）/ 空闲（暗色）
    let (title, border_style) = if app.running {
        (
            Line::from(vec![
                Span::styled(format!("{} ", app.spinner()), theme::busy()),
                Span::styled("运行中 · Esc 取消", theme::busy()),
            ]),
            theme::busy(),
        )
    } else if app.completion().is_some() {
        (
            Line::from(Span::styled("输入 · Tab 补全", theme::accent())),
            theme::accent(),
        )
    } else {
        (
            Line::from(Span::styled("输入 · Enter 发送", theme::dim())),
            theme::dim(),
        )
    };
    let border = Border::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(title);
    let inner = border.inner(area);
    let content = Line::from(vec![
        Span::styled(PROMPT, theme::user_marker()),
        Span::raw(app.input().to_string()),
    ]);
    frame.render_widget(Paragraph::new(content).block(border), area);
    // 光标定位在提示符之后的文本处；单行输入，贴右边界截断（不横向滚动）
    let text_width = inner.width.saturating_sub(PROMPT_WIDTH);
    let x = inner.x + PROMPT_WIDTH + app.cursor_width().min(text_width.saturating_sub(1));
    frame.set_cursor_position(Position::new(x, inner.y));
}

/// 输入框内的提示符。
const PROMPT: &str = "❯ ";
/// 提示符的显示宽度（`❯` + 空格）。
const PROMPT_WIDTH: u16 = 2;

/// 状态栏：左侧模型徽标 + session + 告警；右侧滚动位置 + 键位提示。
fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let session = app
        .session_id
        .as_deref()
        .map_or("无 session".to_string(), |id| {
            format!("session {}", &id[..id.len().min(8)])
        });
    let mut left = vec![
        Span::styled(format!(" {} ", app.model_name), theme::selected()),
        Span::styled(format!(" {session} "), theme::dim()),
    ];
    if let Some(notice) = &app.notice {
        left.push(Span::styled(format!("⚠ {notice} "), theme::warn()));
    }
    let mut right = Vec::new();
    if app.scroll > 0 {
        right.push(Span::styled(
            format!("↑ {}/{} ", app.scroll, app.scroll_max),
            theme::warn(),
        ));
    }
    right.push(Span::styled(
        "/ 命令 · Enter 发送 · Ctrl+C 退出 ",
        theme::dim(),
    ));
    let left_line = Line::from(left);
    let right_line = Line::from(right);
    // 宽度不足时省略右侧提示，避免与左侧信息交叠
    if left_line.width() + right_line.width() <= usize::from(area.width) {
        frame.render_widget(Paragraph::new(right_line).alignment(Alignment::Right), area);
    }
    frame.render_widget(Paragraph::new(left_line), area);
}

/// 补全弹层可见候选数上限，超出时内部滚动窗口。
const COMPLETION_MAX_VISIBLE: usize = 10;

/// 弹层可见窗口：总数超过上限时让选中项大致居中。
fn visible_window(total: usize, selected: usize, max: usize) -> (usize, usize) {
    if total <= max {
        return (0, total);
    }
    let start = selected.saturating_sub(max / 2).min(total - max);
    (start, start + max)
}

/// slash 命令 / skill 名补全弹层：带边框贴在输入框上方，选中项以 `❯` 前缀标出。
fn draw_completion(frame: &mut Frame<'_>, completion: &Completion, input_area: Rect) {
    let total = completion.candidates.len();
    let (start, end) = visible_window(total, completion.selected, COMPLETION_MAX_VISIBLE);
    let lines: Vec<Line<'static>> = completion.candidates[start..end]
        .iter()
        .enumerate()
        .map(|(offset, candidate)| {
            let text = match candidate {
                CompletionCandidate::Command(command) => {
                    format!("/{:<6} {}", command.name, command.summary)
                }
                CompletionCandidate::Skill(entry) => {
                    format!("{:<10} {}", entry.name, entry.description)
                }
            };
            if start + offset == completion.selected {
                Line::from(vec![
                    Span::styled("❯ ", theme::user_marker()),
                    Span::styled(text, theme::accent()),
                ])
            } else {
                Line::from(vec![Span::raw("  "), Span::styled(text, theme::subtle())])
            }
        })
        .collect();
    let kind = match completion.candidates.first() {
        Some(CompletionCandidate::Command(_)) => "命令",
        Some(CompletionCandidate::Skill(_)) => "skill",
        None => "补全",
    };
    let title = if total > COMPLETION_MAX_VISIBLE {
        format!("{kind} {}/{total}", completion.selected + 1)
    } else {
        kind.to_string()
    };
    let block = Border::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::accent())
        .title(Span::styled(title, theme::accent()));
    // 宽度对齐最长候选 + 边框与右留白；高度 = 可见候选 + 上下边框
    let max_line_width = lines
        .iter()
        .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0);
    let width = max_line_width.saturating_add(3).min(input_area.width);
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX) + 2;
    // 贴输入框顶边向上弹出；空间不足时压到聊天区顶部为止
    let y = input_area.y.saturating_sub(height);
    let area = Rect {
        x: input_area.x,
        y,
        width,
        height: height.min(input_area.y),
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// PgUp/PgDn 的滚动步长（供事件循环使用）。
pub(super) const fn page_scroll() -> u16 {
    PAGE_SCROLL
}

#[cfg(test)]
mod tests {
    use nomic_ai::Message;
    use nomic_core::AgentEvent;
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    /// 渲染冒烟：空状态、流式中、工具条目三种画面都能无 panic 绘制。
    #[test]
    fn renders_without_panic() {
        let mut app = App::new(
            "test-model".to_string(),
            Some("abcd1234-session".to_string()),
        );
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::User(
            nomic_ai::UserMessage {
                content: nomic_ai::UserMessageContent::Text("你好".to_string()),
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({"command": "ls -la"}),
        });
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let compact: String = buffer
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(compact.contains("你好"));
        assert!(compact.contains("bash"));
        assert!(compact.contains("test-model"));
        assert!(compact.contains("abcd1234"));
    }

    /// 未定稿的 assistant 消息显示流式指示；运行中输入框标题含 spinner 与提示。
    #[test]
    fn shows_streaming_indicator_and_running_input_state() {
        let mut app = App::new("test-model".to_string(), None);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::Assistant(
            nomic_ai::AssistantMessage {
                content: Vec::new(),
                api: nomic_ai::ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude".to_string(),
                response_model: None,
                response_id: None,
                usage: nomic_ai::Usage::default(),
                stop_reason: nomic_ai::StopReason::Stop,
                error_message: None,
                timestamp: 0,
            },
        ))));
        app.running = true;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let compact: String = buffer
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(compact.contains("生成中"), "{compact}");
        assert!(compact.contains("运行中"), "{compact}");
        assert!(compact.contains(app.spinner()), "{compact}");
    }

    /// 空状态绘制欢迎页：logo、模型名与键位速查均可见。
    #[test]
    fn renders_welcome_when_empty() {
        let mut app = App::new("test-model".to_string(), None);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let compact: String = buffer
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(compact.contains("nomic"), "{compact}");
        assert!(compact.contains("test-model"), "{compact}");
        assert!(compact.contains("/help"), "{compact}");
    }

    /// 补全弹层滚动窗口：不超限全量显示；超限时选中项保持在窗内。
    #[test]
    fn completion_visible_window_keeps_selected_visible() {
        assert_eq!(visible_window(5, 3, 10), (0, 5));
        assert_eq!(visible_window(20, 0, 10), (0, 10));
        let (start, end) = visible_window(20, 12, 10);
        assert!(start <= 12 && 12 < end);
        assert_eq!(end - start, 10);
        // 末尾选中贴底
        assert_eq!(visible_window(20, 19, 10), (10, 20));
    }

    /// 补全弹层与 System 条目也能无 panic 绘制。
    #[test]
    fn renders_completion_popup_and_system_item() {
        let mut app = App::new("test-model".to_string(), None);
        app.push_system(crate::tui::app::help_text());
        for c in "/n".chars() {
            app.insert_char(c);
        }
        assert!(app.completion().is_some());

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let compact: String = buffer
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        // 弹层候选与 System 条目均可见
        assert!(compact.contains("/new"));
        assert!(compact.contains("/help"));
    }
}
