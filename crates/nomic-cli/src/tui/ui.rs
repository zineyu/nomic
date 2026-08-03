//! TUI 渲染：从 [`App`] 状态构建 ratatui 画面（聊天区 + 输入框 + 状态栏）。

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph},
};
use unicode_width::UnicodeWidthChar;

use super::{
    app::{
        App, Block, ChatItem, Completion, CompletionCandidate, Picker, PickerKind, ToolItem,
        ToolStatus,
    },
    markdown, theme,
};

/// 聊天区左右留白列数，避免输出紧贴屏幕边缘。
const CHAT_H_MARGIN: u16 = 1;

/// 绘制整帧。
pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(input_height(app)),
        Constraint::Length(1),
    ])
    .split(frame.area());
    // Paragraph 不清理文本以外的单元格；先 Clear 避免长行残留
    frame.render_widget(Clear, frame.area());
    draw_chat(frame, app, chunks[0].inner(Margin::new(CHAT_H_MARGIN, 0)));
    draw_input(frame, app, chunks[1]);
    draw_status(frame, app, chunks[2]);
    if let Some(completion) = app.completion() {
        draw_completion(frame, completion, chunks[1]);
    }
    if let Some(picker) = app.picker() {
        draw_picker(frame, picker, chunks[1]);
    }
}

/// 聊天区：历史条目 + 流式累积，软换行，`scroll` 从底部向上计。
fn draw_chat(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    if app.items().is_empty() {
        app.clamp_scroll(0);
        draw_welcome(frame, app, area);
        return;
    }
    let spinner = app.spinner();
    // 每个 MessageBlock 渲染为一组物理行，组间统一空行分隔：用户消息、
    // assistant 的 Text/Thinking、错误、System、工具调用都是
    // 独立消息块，块间空行由拼接处保证，而非各分支自行追加。
    // 运行中状态由输入框标题（spinner + 「运行中 · Esc 取消」）统一表达，
    // 聊天区不再叠加流式指示，避免思考时出现两处"生成中"标记。
    let mut blocks: Vec<Vec<Line<'static>>> = Vec::new();
    for item in app.items() {
        match item {
            ChatItem::User(text) => {
                // 左侧 accent 竖条把整条用户消息包成视觉块，多轮对话里可扫读
                if text.lines().next().is_none() {
                    // 空消息保留竖条占位，保证可见
                    blocks.push(vec![Line::from(Span::styled("▌", theme::user_marker()))]);
                } else {
                    let mut block = MessageBlock::new(theme::user_marker());
                    for line in text.lines() {
                        block.push(Line::from(Span::styled(
                            line.to_string(),
                            theme::user_text(),
                        )));
                    }
                    blocks.push(block.render(area.width));
                }
            }
            ChatItem::Assistant(assistant) => {
                for block in &assistant.blocks {
                    match block {
                        Block::Text(text) => {
                            // assistant 输出按 Markdown 渲染，宽度扣除 gutter 两列
                            let mut message = MessageBlock::new(theme::assistant_marker());
                            for line in
                                markdown::render(text, MessageBlock::content_width(area.width))
                            {
                                message.push(line);
                            }
                            blocks.push(message.render(area.width));
                        }
                        Block::Thinking(thinking) => {
                            // 同一消息块组件，暗色竖条 + 斜体正文与 assistant 输出区分，
                            // 不加标题行，思考内容直接以 gutter 竖条表达
                            let mut message = MessageBlock::new(theme::thinking_marker());
                            for line in thinking.lines() {
                                message.push(Line::from(Span::styled(
                                    line.to_string(),
                                    theme::thinking(),
                                )));
                            }
                            blocks.push(message.render(area.width));
                        }
                    }
                }
                if let Some(error) = &assistant.error {
                    let mut message = MessageBlock::new(theme::err());
                    message.push(Line::from(Span::styled(format!("✗ {error}"), theme::err())));
                    blocks.push(message.render(area.width));
                }
            }
            ChatItem::System(text) => {
                let mut block = MessageBlock::new(theme::dim());
                for line in text.lines() {
                    block.push(Line::from(Span::styled(line.to_string(), theme::dim())));
                }
                blocks.push(block.render(area.width));
            }
            ChatItem::Tool(tool) => {
                blocks.push(tool_block(tool, spinner).render(area.width));
            }
        }
    }
    // 拼接：每个消息块后空一行，块间分隔与末尾留白（与输入框拉开距离）统一处理
    let mut lines: Vec<Line<'static>> = Vec::new();
    for block in blocks {
        lines.extend(block);
        lines.push(Line::default());
    }
    if app.items().is_empty() {
        lines.push(Line::from(Span::styled(
            "输入 prompt 开始对话。Enter 发送，Ctrl+C 退出。",
            theme::dim(),
        )));
    }

    // 自行折行（硬换行，CJK 友好），使行数精确可知、滚动偏移精确
    let lines = wrap_lines(&lines, area.width);
    let total = lines.len();
    let max_scroll =
        u16::try_from(total.saturating_sub(usize::from(area.height))).unwrap_or(u16::MAX);
    // 钳制滚动偏移并同步上限（状态栏滚动位置显示），取生效偏移渲染
    let scroll = app.clamp_scroll(max_scroll);
    let offset = max_scroll.saturating_sub(scroll);
    let paragraph = Paragraph::new(lines).scroll((offset, 0));
    frame.render_widget(paragraph, area);
}

/// 消息块组件：聊天区每条消息的视觉单元，gutter 竖条是组件的一部分。
///
/// 聊天区所有条目（用户消息、assistant 输出、thinking、工具调用、
/// System 提示、错误与流式状态行）都包成 `MessageBlock`，仅靠竖条与
/// 正文颜色区分条目类型（用户=accent、assistant=正文色、thinking=暗色、
/// 工具=状态色、System=暗色、错误=红、流式=黄）。
///
/// 组件内部统一负责折行：正文宽度为总宽减去 gutter 两列，续行自动
/// 延续竖条，块引用视觉不断裂；空行（段落间隔）同样延续竖条，
/// 竖条覆盖整个消息块，形成完整的左侧边框。
struct MessageBlock {
    /// gutter 竖条样式（颜色区分条目类型）。
    marker: Style,
    /// 正文逻辑行（未加竖条、未折行）。
    lines: Vec<Line<'static>>,
}

/// gutter 竖条前缀：每条物理行的行首。
const GUTTER_PREFIX: &str = "▌ ";
/// gutter 占用列数：`▌` + 空格。
const GUTTER_WIDTH: u16 = 2;

impl MessageBlock {
    const fn new(marker: Style) -> Self {
        Self {
            marker,
            lines: Vec::new(),
        }
    }

    /// 追加一行正文（逻辑行，折行由组件负责）。
    fn push(&mut self, line: Line<'static>) {
        self.lines.push(line);
    }

    /// 正文可用宽度：总宽减去 gutter 两列。
    fn content_width(width: u16) -> u16 {
        width.saturating_sub(GUTTER_WIDTH).max(1)
    }

    /// 渲染为物理行：每行加 gutter 竖条并按显示宽度折行，续行延续竖条。
    fn render(&self, width: u16) -> Vec<Line<'static>> {
        let max = usize::from(Self::content_width(width));
        let mut out = Vec::new();
        for line in &self.lines {
            if line.width() == 0 {
                // 空行（段落间隔）同样加竖条，保证竖条覆盖完整消息块
                out.push(self.with_gutter(Line::default()));
                continue;
            }
            for wrapped in wrap_line(line, max) {
                out.push(self.with_gutter(wrapped));
            }
        }
        out
    }

    /// 给物理行行首加 gutter 竖条。
    fn with_gutter(&self, line: Line<'static>) -> Line<'static> {
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        spans.push(Span::styled(GUTTER_PREFIX, self.marker));
        spans.extend(line.spans);
        Line::from(spans)
    }
}

/// 工具条目组件：gutter 竖条取状态色，状态标记 + 加粗工具名 + 暗色 (参数)，
/// 结果摘要首行 `⎿` 引导、后续行对齐缩进，保持树形层次。
fn tool_block(tool: &ToolItem, spinner: &str) -> MessageBlock {
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
    let mut block = MessageBlock::new(mark_style);
    block.push(Line::from(spans));
    if !tool.detail.is_empty() {
        let detail_style = if tool.status == ToolStatus::Failed {
            theme::err()
        } else {
            theme::dim()
        };
        for (index, detail) in tool.detail.iter().enumerate() {
            let prefix = if index == 0 { "  ⎿ " } else { "    " };
            block.push(Line::from(Span::styled(
                format!("{prefix}{detail}"),
                detail_style,
            )));
        }
    }
    block
}

/// 空状态欢迎页：居中 logo + 键位速查。
fn draw_welcome(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            format!("▌ nomic v{}", env!("CARGO_PKG_VERSION")),
            theme::user_marker(),
        )),
        Line::from(Span::styled(
            format!("agent TUI · {}", app.model_name()),
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

/// 把单个逻辑行按显示宽度折成物理行（保留 span 样式）。
fn wrap_line(line: &Line<'static>, max: usize) -> Vec<Line<'static>> {
    let max = max.max(1);
    let mut out = Vec::new();
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
    out
}

/// 把逻辑行按显示宽度折成物理行（保留 span 样式）。
///
/// 仅用于组件外的裸行（条目间留白、空列表提示）；消息行的折行由
/// [`MessageBlock::render`] 负责，输出已适配宽度，此处幂等。
fn wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let max = usize::from(width).max(1);
    lines.iter().flat_map(|line| wrap_line(line, max)).collect()
}

/// 输入框内容区行数上限：高度随行数伸缩，超过后内部滚动。
const MAX_INPUT_LINES: u16 = 5;

/// 输入框总高度（含上下边框）：附件行（可选）+ 1..=5 行内容 + 2 行边框。
fn input_height(app: &App) -> u16 {
    app.line_count().min(MAX_INPUT_LINES) + 2 + u16::from(app.has_attachments())
}

/// 输入框（多行，高度随行数变化，最多 5 行）+ 光标定位。
fn draw_input(frame: &mut Frame<'_>, app: &App, area: Rect) {
    // 三态边框：运行中（黄 + spinner）/ 补全打开（accent）/ 空闲（暗色）
    let (title, border_style) = if app.is_running() {
        (
            Line::from(vec![
                Span::styled(format!("{} ", app.spinner()), theme::busy()),
                Span::styled("运行中 · Esc 取消", theme::busy()),
            ]),
            theme::busy(),
        )
    } else if let Some(picker) = app.picker() {
        let title = match picker.kind {
            PickerKind::Resume => "恢复 session · ↑/↓ 选择 · Enter 确认 · Esc 取消",
            PickerKind::Tree => "会话树 · ↑/↓ 选择 · Enter 创建分支 · Esc 取消",
            PickerKind::Models => "切换模型 · ↑/↓ 选择 · Enter 确认 · Esc 取消",
        };
        (
            Line::from(Span::styled(title, theme::accent())),
            theme::accent(),
        )
    } else if app.completion().is_some() {
        (
            Line::from(Span::styled("输入 · Tab 补全", theme::accent())),
            theme::accent(),
        )
    } else {
        (
            Line::from(Span::styled(
                "输入 · Enter 发送 · Shift+Enter 换行",
                theme::dim(),
            )),
            theme::dim(),
        )
    };
    let border = Border::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(title);
    let inner = border.inner(area);
    // 附件行（可选）在输入文本上方：🖼 文件名列表
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.has_attachments() {
        let names = app.attachment_names().collect::<Vec<_>>().join(" · ");
        lines.push(Line::from(Span::styled(
            format!("🖼 {names}"),
            theme::accent(),
        )));
    }
    lines.extend(
        app.input()
            .split('\n')
            .map(|text| Line::from(Span::raw(text.to_string()))),
    );
    // 行数超过可见高度时滚动到光标所在行
    let attachment_offset = u16::from(app.has_attachments());
    let (cursor_row, cursor_col) = app.cursor_position();
    let cursor_row = cursor_row + attachment_offset;
    let visible = inner.height.max(1);
    let scroll = cursor_row.saturating_sub(visible - 1);
    frame.render_widget(
        Paragraph::new(lines).block(border).scroll((scroll, 0)),
        area,
    );
    // 光标定位在文本处；长行贴右边界截断（不横向滚动）
    let x = inner.x + cursor_col.min(inner.width.saturating_sub(1));
    let y = inner.y + (cursor_row - scroll).min(visible - 1);
    frame.set_cursor_position(Position::new(x, y));
}

/// 状态栏：左侧模型徽标 + session + 上下文用量 + 告警；右侧滚动位置 + 键位提示。
fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let session = app.session_id().map_or_else(
        || "无 session".to_string(),
        |id| format!("session {}", &id[..id.len().min(8)]),
    );
    let mut left = vec![
        Span::styled(format!(" {} ", app.model_name()), theme::selected()),
        Span::styled(format!(" {session} "), theme::dim()),
        context_usage_span(app),
    ];
    if let Some(notice) = app.notice() {
        left.push(Span::styled(format!("⚠ {notice} "), theme::warn()));
    }
    let mut right = Vec::new();
    if app.scroll() > 0 {
        right.push(Span::styled(
            format!("↑ {}/{} ", app.scroll(), app.scroll_max()),
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

/// 状态栏上下文用量：`ctx 12.3k/200k·6%`；窗口未知（0）时不显示占比。
/// 用量逼近窗口（≥80%）时以警告色提示。
fn context_usage_span(app: &App) -> Span<'static> {
    let tokens = app.context_tokens();
    let window = app.context_window();
    if window == 0 {
        return Span::styled(format!(" ctx {} ", format_tokens(tokens)), theme::dim());
    }
    let percent = tokens.saturating_mul(100) / window;
    let text = format!(
        " ctx {}/{}·{}% ",
        format_tokens(tokens),
        format_tokens(window),
        percent
    );
    let style = if percent >= 80 {
        theme::warn()
    } else {
        theme::dim()
    };
    Span::styled(text, style)
}

/// token 数紧凑格式：<1k 原样，<10k 一位小数（`8.4k`），其余取整（`200k`）。
pub(super) fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 10_000 {
        let deci_k = tokens / 100;
        format!("{}.{}k", deci_k / 10, deci_k % 10)
    } else {
        format!("{}k", tokens / 1_000)
    }
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
                CompletionCandidate::Template(template) => match &template.argument_hint {
                    Some(hint) => format!(
                        "/{:<8} {:<14} {}",
                        template.name, hint, template.description
                    ),
                    None => format!("/{:<8} {}", template.name, template.description),
                },
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
        Some(CompletionCandidate::Template(_)) => "模板",
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

/// 选择器弹层（`/resume` / `/models` 共用）：与补全弹层同构，贴在输入框上方。
fn draw_picker(frame: &mut Frame<'_>, picker: &Picker, input_area: Rect) {
    let total = picker.rows.len();
    let (start, end) = visible_window(total, picker.selected, COMPLETION_MAX_VISIBLE);
    let lines: Vec<Line<'static>> = picker.rows[start..end]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            if start + offset == picker.selected {
                Line::from(vec![
                    Span::styled("❯ ", theme::user_marker()),
                    Span::styled(row.text.clone(), theme::accent()),
                ])
            } else {
                // 不可选行（`/tree` 的工具调用条目）再降一档，仅作浏览上下文
                let style = if row.selectable {
                    theme::subtle()
                } else {
                    theme::dim()
                };
                Line::from(vec![Span::raw("  "), Span::styled(row.text.clone(), style)])
            }
        })
        .collect();
    let action = match picker.kind {
        PickerKind::Resume => "恢复 session",
        PickerKind::Tree => "会话树",
        PickerKind::Models => "切换模型",
    };
    let title = if total > COMPLETION_MAX_VISIBLE {
        format!("{action} {}/{total}", picker.selected + 1)
    } else {
        action.to_string()
    };
    let block = Border::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::accent())
        .title(Span::styled(title, theme::accent()));
    // 宽度对齐最长行 + 边框与右留白；高度 = 可见行 + 上下边框
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

#[cfg(test)]
mod tests {
    use nomic_ai::Message;
    use nomic_core::{AgentEvent, ToolResult};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    /// 渲染冒烟：空状态、流式中、工具条目三种画面都能无 panic 绘制。
    #[test]
    fn renders_without_panic() {
        let mut app = App::new(
            "test-model".to_string(),
            Some("abcd1234-session".to_string()),
            200_000,
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

    /// 运行中输入框标题含 spinner 与提示（聊天区不再叠加流式指示）。
    #[test]
    fn shows_running_input_state() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
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
        app.handle_event(&AgentEvent::AgentStart);

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
        assert!(!compact.contains("生成中"), "{compact}");
        assert!(compact.contains("运行中"), "{compact}");
        assert!(compact.contains(app.spinner()), "{compact}");
    }

    /// 工具条目树形渲染：参数用语义摘要，多行结果首行 `⎿`、后续行对齐。
    #[test]
    fn renders_tool_tree_with_multiline_detail() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({"command": "cargo test"}),
        });
        app.handle_event(&AgentEvent::ToolExecutionEnd {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            result: ToolResult::text("line1\nline2\nline3"),
            is_error: false,
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let rows: Vec<String> = buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect())
            .collect();
        let text = rows.join("\n");
        assert!(text.contains("bash(cargo test)"), "{text}");
        assert!(text.contains("  ⎿ line1"), "{text}");
        assert!(text.contains("    line2"), "{text}");
        assert!(text.contains("    line3"), "{text}");
    }

    /// 各条目共用 `▌` gutter 组件，但颜色不同：用户=accent，工具=状态色。
    #[test]
    fn gutter_colors_distinguish_item_types() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::User(
            nomic_ai::UserMessage {
                content: nomic_ai::UserMessageContent::Text("用户消息".to_string()),
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({"command": "ls"}),
        });
        app.handle_event(&AgentEvent::ToolExecutionEnd {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            result: ToolResult::text("ok"),
            is_error: false,
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let gutter_colors: Vec<ratatui::style::Color> = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "▌")
            .map(|cell| cell.fg)
            .collect();
        assert!(
            gutter_colors.contains(&ratatui::style::Color::Cyan),
            "{gutter_colors:?}"
        );
        assert!(
            gutter_colors.contains(&ratatui::style::Color::Green),
            "{gutter_colors:?}"
        );
    }

    /// System 条目与错误/流式状态行也套用 gutter 组件：System=暗色，错误=红色。
    #[test]
    fn system_and_error_lines_use_gutter() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.push_system("本地系统提示");
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
        app.handle_event(&AgentEvent::MessageEnd(Box::new(Message::Assistant(
            nomic_ai::AssistantMessage {
                content: Vec::new(),
                api: nomic_ai::ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude".to_string(),
                response_model: None,
                response_id: None,
                usage: nomic_ai::Usage::default(),
                stop_reason: nomic_ai::StopReason::Error,
                error_message: Some("rate limited".to_string()),
                timestamp: 0,
            },
        ))));

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
        assert!(compact.contains("▌本地系统提示"), "{compact}");
        assert!(compact.contains("▌✗ratelimited"), "{compact}");
        let gutter_colors: Vec<ratatui::style::Color> = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "▌")
            .map(|cell| cell.fg)
            .collect();
        assert!(
            gutter_colors.contains(&ratatui::style::Color::DarkGray),
            "{gutter_colors:?}"
        );
        assert!(
            gutter_colors.contains(&ratatui::style::Color::Red),
            "{gutter_colors:?}"
        );
    }

    /// thinking 块套用 gutter 组件：无标题，`▌` 竖条正文，颜色区别于其他条目。
    #[test]
    fn renders_thinking_block_with_gutter() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
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
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::ThinkingStart { index: 0 },
        ));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::ThinkingDelta {
                index: 0,
                delta: "推理第一行\n推理第二行".to_string(),
            },
        ));

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
        assert!(!compact.contains("Thinking"), "{compact}");
        assert!(compact.contains("▌推理第一行"), "{compact}");
        assert!(compact.contains("▌推理第二行"), "{compact}");
    }

    /// 空状态绘制欢迎页：logo、模型名与键位速查均可见。
    #[test]
    fn renders_welcome_when_empty() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
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

    /// `/resume` 选择器弹层：session 行、标题与选中标记均可见。
    #[test]
    fn renders_resume_picker() {
        use super::super::app::PickerRow;

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.open_resume_picker(vec![
            PickerRow {
                selectable: true,
                id: "01999999-aaaa".to_string(),
                text: "01999999  2026-07-26 14:48    3 条消息  /tmp/a".to_string(),
            },
            PickerRow {
                selectable: true,
                id: "02888888-bbbb".to_string(),
                text: "02888888  2026-07-25 09:00   12 条消息  /tmp/b".to_string(),
            },
        ]);

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
        assert!(compact.contains("恢复session"), "{compact}");
        assert!(compact.contains("01999999"), "{compact}");
        assert!(compact.contains("02888888"), "{compact}");
    }

    /// `/models` 选择器弹层：标题与模型行可见，预选中当前模型。
    #[test]
    fn renders_model_picker() {
        use super::super::app::PickerRow;

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.open_model_picker(
            vec![
                PickerRow {
                    selectable: true,
                    id: "claude-sonnet-4-5".to_string(),
                    text: "claude-sonnet-4-5 — Claude Sonnet 4.5 · ctx 200k".to_string(),
                },
                PickerRow {
                    selectable: true,
                    id: "claude-opus-4-7".to_string(),
                    text: "claude-opus-4-7 — Claude Opus 4.7 · ctx 200k（当前）".to_string(),
                },
            ],
            1,
        );

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
        assert!(compact.contains("切换模型"), "{compact}");
        assert!(compact.contains("claude-sonnet-4-5"), "{compact}");
        assert!(compact.contains("claude-opus-4-7"), "{compact}");
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

    /// assistant 文本块按 Markdown 渲染：标记符号不原样上屏，样式落到 cell。
    #[test]
    fn renders_assistant_markdown_with_styles() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
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
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::TextStart { index: 0 },
        ));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::TextDelta {
                index: 0,
                delta: "# 标题\n\n- 项一\n- 项二".to_string(),
            },
        ));

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
        // Markdown 标记不原样上屏，列表项带渲染后的符号
        assert!(compact.contains("标题"), "{compact}");
        assert!(!compact.contains("#标题"), "{compact}");
        assert!(compact.contains("•项一"), "{compact}");
        // 标题 cell 带加粗样式
        let heading_cell = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "标")
            .expect("heading cell");
        assert!(
            heading_cell
                .modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    /// 聊天区左右留白：assistant 输出不紧贴屏幕左缘。
    #[test]
    fn chat_content_has_left_margin() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
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
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::TextStart { index: 0 },
        ));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::TextDelta {
                index: 0,
                delta: "输出内容".to_string(),
            },
        ));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let index = buffer
            .content()
            .iter()
            .position(|cell| cell.symbol() == "输")
            .expect("assistant text cell");
        let x = u16::try_from(index % width).expect("column fits u16");
        // assistant 输出套用 gutter 组件：留白 + `▌ ` 竖条两列
        assert_eq!(
            x,
            CHAT_H_MARGIN + 2,
            "assistant 输出应距左缘 {} 列",
            CHAT_H_MARGIN + 2
        );
    }

    /// 补全弹层与 System 条目也能无 panic 绘制。
    #[test]
    fn renders_completion_popup_and_system_item() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.push_system("本地系统提示");
        app.paste_text("/n");
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
        assert!(compact.contains("本地系统提示"));
    }

    /// 消息块组件：折行后续行保留 gutter 竖条，块引用视觉不断裂，每行宽度不超上限。
    #[test]
    fn message_block_wraps_with_continuous_gutter() {
        let style = theme::user_marker();
        let mut block = MessageBlock::new(style);
        block.push(Line::from(Span::styled(
            "一二三四五六七八九十",
            theme::user_text(),
        )));
        let rendered = block.render(8);
        // 竖条 2 列 + 每行 6 列内容（3 个 CJK 字）：10 字折 4 行
        assert_eq!(rendered.len(), 4, "{rendered:?}");
        for line in &rendered {
            assert!(line.width() <= 8, "折行后宽度应 <= 8：{:?}", line.width());
            let first = line.spans.first().expect("续行应有 gutter 竖条");
            assert_eq!(first.content.as_ref(), GUTTER_PREFIX);
            assert_eq!(first.style, style, "续行竖条应保持原 gutter 颜色");
        }
    }

    /// 消息块组件：空行（段落间隔）同样延续竖条，竖条覆盖完整消息块。
    #[test]
    fn message_block_extends_gutter_over_blank_lines() {
        let style = theme::dim();
        let mut block = MessageBlock::new(style);
        block.push(Line::from(Span::raw("上段")));
        block.push(Line::default());
        block.push(Line::from(Span::raw("下段")));
        let rendered = block.render(20);
        assert_eq!(rendered.len(), 3);
        let blank = &rendered[1];
        let first = blank.spans.first().expect("空行也应有 gutter 竖条");
        assert_eq!(first.content.as_ref(), GUTTER_PREFIX);
        assert_eq!(first.style, style, "空行竖条应保持原 gutter 颜色");
        assert_eq!(blank.width(), usize::from(GUTTER_WIDTH));
    }

    /// 聊天区拼接：相邻消息块之间恰好空一行（工具/System/用户等任意组合）。
    #[test]
    fn chat_blocks_are_separated_by_blank_lines() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.push_system("第一条");
        app.push_system("第二条");

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let row_of = |needle: &str| {
            buffer
                .content()
                .iter()
                .position(|cell| cell.symbol() == needle)
                .map(|index| index / width)
        };
        let first = row_of("第").expect("first system row");
        let second = row_of("二").expect("second system row");
        // 两个单行消息块之间恰好一行空白
        assert_eq!(second - first, 2, "相邻消息块之间应空一行");
        // 分隔行是真空行：整行无内容字符
        let blank_row = &buffer.content()[(first + 1) * width..(first + 2) * width];
        assert!(
            blank_row.iter().all(|cell| cell.symbol() == " "),
            "分隔行应为空白行"
        );
    }

    /// 裸行折行：无 gutter 前缀，行为不变。
    #[test]
    fn wrapped_plain_lines_have_no_gutter() {
        let line = Line::from(Span::raw("abcdefgh"));
        let wrapped = wrap_lines(&[line], 4);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].spans[0].content.as_ref(), "abcd");
        assert_eq!(wrapped[1].spans[0].content.as_ref(), "efgh");
    }

    /// token 数紧凑格式：<1k 原样，<10k 一位小数，其余取整。
    #[test]
    fn formats_tokens_compactly() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(843), "843");
        assert_eq!(format_tokens(1_000), "1.0k");
        assert_eq!(format_tokens(8_432), "8.4k");
        assert_eq!(format_tokens(12_300), "12k");
        assert_eq!(format_tokens(200_000), "200k");
    }

    /// 状态栏显示上下文用量：token 估算 / 窗口 / 占比。
    #[test]
    fn status_bar_shows_context_usage() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.set_context_tokens(8_432);

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
        assert!(compact.contains("ctx8.4k/200k·4%"), "{compact}");
    }
}
