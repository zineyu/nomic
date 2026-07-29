//! TUI 渲染：从 [`App`] 状态构建 ratatui 画面（聊天区 + 输入框 + 状态栏）。

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block as Border, Clear, Paragraph},
};
use unicode_width::UnicodeWidthChar;

use super::app::{App, Block, ChatItem, Completion, CompletionCandidate, ToolStatus};

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
    let mut lines: Vec<Line<'static>> = Vec::new();
    for item in &app.items {
        match item {
            ChatItem::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "❯ ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(text.clone(), Style::default().fg(Color::Cyan)),
                ]));
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
                                Line::from(Span::styled(
                                    line.to_string(),
                                    Style::default()
                                        .fg(Color::DarkGray)
                                        .add_modifier(Modifier::ITALIC),
                                ))
                            }));
                        }
                    }
                }
                if let Some(error) = &assistant.error {
                    lines.push(Line::from(Span::styled(
                        format!("✗ {error}"),
                        Style::default().fg(Color::Red),
                    )));
                }
                if !assistant.blocks.is_empty() || assistant.error.is_some() {
                    lines.push(Line::default());
                }
            }
            ChatItem::System(text) => {
                lines.extend(text.lines().map(|line| {
                    Line::from(Span::styled(
                        line.to_string(),
                        Style::default().fg(Color::DarkGray),
                    ))
                }));
                lines.push(Line::default());
            }
            ChatItem::Tool(tool) => {
                let (mark, color) = match tool.status {
                    ToolStatus::Running => ("▶", Color::Yellow),
                    ToolStatus::Ok => ("✓", Color::Green),
                    ToolStatus::Failed => ("✗", Color::Red),
                };
                let mut spans = vec![
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::styled(
                        tool.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ];
                if !tool.args.is_empty() {
                    spans.push(Span::styled(
                        format!(" {}", tool.args),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                lines.push(Line::from(spans));
                if let Some(detail) = &tool.detail {
                    lines.push(Line::from(Span::styled(
                        format!("  ⎿ {detail}"),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }
    }
    if app.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "输入 prompt 开始对话。Enter 发送，Ctrl+C 退出。",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // 自行折行（硬换行，CJK 友好），使行数精确可知、滚动偏移精确
    let lines = wrap_lines(&lines, area.width);
    let total = lines.len();
    let max_scroll = total.saturating_sub(usize::from(area.height));
    app.scroll = app
        .scroll
        .min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
    let offset = max_scroll.saturating_sub(usize::from(app.scroll));
    let offset = u16::try_from(offset).unwrap_or(u16::MAX);
    let paragraph = Paragraph::new(lines).scroll((offset, 0));
    frame.render_widget(paragraph, area);
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
    let title = if app.running {
        "运行中…（Esc 取消）"
    } else {
        "输入（Enter 发送）"
    };
    let border = Border::bordered().title(title);
    let inner = border.inner(area);
    frame.render_widget(Paragraph::new(app.input().to_string()).block(border), area);
    // 光标定位在框内文本处；单行输入，宽度即列偏移（贴边截断，最小版不横向滚动）
    let x = inner.x + app.cursor_width().min(inner.width.saturating_sub(1));
    frame.set_cursor_position(Position::new(x, inner.y));
}

/// 状态栏：模型 / session / 滚动提示 / 告警。
fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let session = app
        .session_id
        .as_deref()
        .map_or("无 session".to_string(), |id| {
            format!("session {}", &id[..id.len().min(8)])
        });
    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.model_name),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {session} "), Style::default().fg(Color::DarkGray)),
    ];
    if app.scroll > 0 {
        spans.push(Span::styled(
            format!("↑ 上滚 {} 行（PgDn 回到底部） ", app.scroll),
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(notice) = &app.notice {
        spans.push(Span::styled(
            format!("⚠ {notice} "),
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// slash 命令 / skill 名补全弹层：贴在输入框上方，选中项高亮。
fn draw_completion(frame: &mut Frame<'_>, completion: &Completion, input_area: Rect) {
    let lines: Vec<Line<'static>> = completion
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let style = if index == completion.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            let text = match candidate {
                CompletionCandidate::Command(command) => {
                    format!("/{:<6} {}", command.name, command.summary)
                }
                CompletionCandidate::Skill(entry) => {
                    format!("/skill:{:<6} {}", entry.name, entry.description)
                }
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let width = lines
        .iter()
        .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0)
        .min(input_area.width);
    // 贴输入框顶边向上弹出；空间不足时压到聊天区顶部为止
    let y = input_area.y.saturating_sub(height);
    let area = Rect {
        x: input_area.x,
        y,
        width,
        height: height.min(input_area.y),
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines), area);
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
