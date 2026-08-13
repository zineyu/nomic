//! TUI 渲染：从 [`App`] 状态构建 ratatui 画面（聊天区 + 输入框 + 状态栏）。

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Margin, Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{
    app::{
        App, AssistantItem, Block, ChatItem, Completion, CompletionCandidate, CopyMenu, Mode,
        Picker, PickerKind, ToolItem, ToolStatus,
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
    if let Some(completion) = app.command().completion() {
        draw_completion(frame, completion, chunks[1]);
    }
    if let Some(picker) = app.picker() {
        draw_picker(frame, picker, chunks[1]);
    }
    // 复制菜单与帮助弹层是模态覆盖层：内容区（状态栏以上）整体作为画布
    let content = Rect {
        height: frame.area().height.saturating_sub(1),
        ..frame.area()
    };
    if let Some(menu) = app.copy_menu() {
        draw_copy_menu(frame, menu, content);
    }
    if app.help_open() {
        draw_help(frame, app, content);
    }
}

/// 聊天区：历史条目 + 流式累积，软换行，`scroll` 从底部向上计。
fn draw_chat(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    if app.chat().items().is_empty() {
        app.chat_mut().clamp_scroll(0);
        draw_welcome(frame, app, area);
        return;
    }
    let cursor = app.chat_cursor();
    let (lines, starts) = chat_lines(app, area.width, cursor);
    // 自行折行（硬换行，CJK 友好），使行数精确可知、滚动偏移精确
    let lines = wrap_lines(&lines, area.width);
    // 搜索命中高亮：Enter 后保留（Esc 清空搜索串即消除）
    let lines = if let Some(query) = app.search().highlight() {
        lines
            .iter()
            .map(|line| highlight_line(line, query, theme::search_hit()))
            .collect()
    } else {
        lines
    };
    let total = lines.len();
    let max_scroll =
        u16::try_from(total.saturating_sub(usize::from(area.height))).unwrap_or(u16::MAX);
    // 钳制滚动偏移并同步上限（状态栏滚动位置显示），取生效偏移渲染
    let scroll = app.chat_mut().clamp_scroll(max_scroll);
    app.chat_mut().sync_item_lines(starts);
    let offset = max_scroll.saturating_sub(scroll);
    let paragraph = Paragraph::new(lines).scroll((offset, 0));
    frame.render_widget(paragraph, area);
}

/// 组装聊天区逻辑行：完整消息块 + 块间空行；游标条目整行高亮
///（gutter 换符号与样式、行铺背景并补齐行宽）。
/// 返回（逻辑行，各条目起始行——消息游标滚动定位用，回写状态层）。
fn chat_lines(app: &App, width: u16, cursor: Option<usize>) -> (Vec<Line<'static>>, Vec<u16>) {
    let spinner = app.spinner();
    // 每个块标注所属条目下标：游标 gutter 高亮与条目起始行回写用
    let mut blocks: Vec<(usize, Vec<Line<'static>>)> = Vec::new();
    for (index, item) in app.chat().items().iter().enumerate() {
        for block in item_blocks(item, width, app.thinking_collapsed(), spinner) {
            blocks.push((index, block));
        }
    }
    // 拼接：每个消息块后空一行，块间分隔与末尾留白（与输入框拉开距离）统一处理；
    // 同时记录各条目起始行。块间隔空行同样延续高亮，形成连续的高亮区。
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut starts = vec![u16::MAX; app.chat().items().len()];
    for (index, block) in blocks {
        if starts[index] == u16::MAX {
            starts[index] = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        }
        let highlight = (cursor == Some(index)).then(theme::cursor_marker);
        let block = if let Some(marker) = highlight {
            block
                .into_iter()
                .map(|line| restyle_highlight(line, marker, GUTTER_CURSOR_BODY, width))
                .collect::<Vec<_>>()
        } else {
            block
        };
        lines.extend(block);
        // 块间隔空行：高亮条目的空行延续 gutter 与背景，高亮区不断裂
        let gap = if let Some(marker) = highlight {
            restyle_highlight(Line::default(), marker, GUTTER_CURSOR_BODY, width)
        } else {
            Line::default()
        };
        lines.push(gap);
    }
    (lines, starts)
}

/// 单个聊天条目渲染为消息块列表（每块是一组带 gutter 的物理行）；
/// 运行中工具经 `spinner` 传帧。assistant/tool 条目折叠时渲染为单行摘要。
fn item_blocks(
    item: &ChatItem,
    width: u16,
    thinking_collapsed: bool,
    spinner: &str,
) -> Vec<Vec<Line<'static>>> {
    let collapsed = match item {
        ChatItem::Assistant(assistant) => assistant.collapsed,
        ChatItem::Tool(tool) => tool.collapsed,
        ChatItem::User(_) | ChatItem::System(_) => false,
    };
    if collapsed {
        return item_summary(item, width, spinner);
    }
    let mut blocks = Vec::new();
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
                blocks.push(block.render(width));
            }
        }
        ChatItem::Assistant(assistant) => {
            for block in &assistant.blocks {
                match block {
                    Block::Text(text) => {
                        // assistant 输出按 Markdown 渲染，宽度扣除 gutter 两列
                        let mut message = MessageBlock::new(theme::assistant_marker());
                        for line in markdown::render(text, MessageBlock::content_width(width)) {
                            message.push(line);
                        }
                        blocks.push(message.render(width));
                    }
                    Block::Thinking(thinking) => {
                        // 同一消息块组件，暗色竖条 + 斜体正文与 assistant 输出区分，
                        // 不加标题行，思考内容直接以 gutter 竖条表达；
                        // 折叠时只渲染一行占位（`/thinking` 切换）
                        let mut message = MessageBlock::new(theme::thinking_marker());
                        if thinking_collapsed {
                            let count = thinking.lines().count();
                            message.push(Line::from(Span::styled(
                                format!("思考过程（{count} 行，已折叠；/thinking 展开）"),
                                theme::thinking(),
                            )));
                        } else {
                            for line in thinking.lines() {
                                message.push(Line::from(Span::styled(
                                    line.to_string(),
                                    theme::thinking(),
                                )));
                            }
                        }
                        blocks.push(message.render(width));
                    }
                }
            }
            if let Some(error) = &assistant.error {
                let mut message = MessageBlock::new(theme::err());
                message.push(Line::from(Span::styled(format!("✗ {error}"), theme::err())));
                blocks.push(message.render(width));
            }
        }
        ChatItem::System(text) => {
            let mut block = MessageBlock::new(theme::dim());
            for line in text.lines() {
                block.push(Line::from(Span::styled(line.to_string(), theme::dim())));
            }
            blocks.push(block.render(width));
        }
        ChatItem::Tool(tool) => {
            blocks.push(tool_block(tool, spinner).render(width));
        }
    }
    blocks
}

/// 条目折叠摘要行（NORMAL `Space`）：类型 gutter + 首行摘要，超长截断为
/// `…`；完整内容仍可经复制菜单（`y`）复制，不受折叠影响。
fn item_summary(item: &ChatItem, width: u16, spinner: &str) -> Vec<Vec<Line<'static>>> {
    let (marker, content) = match item {
        ChatItem::User(text) => (
            theme::user_marker(),
            Line::from(Span::styled(first_line(text), theme::user_text())),
        ),
        ChatItem::Assistant(assistant) => (theme::assistant_marker(), assistant_summary(assistant)),
        ChatItem::System(text) => (
            theme::dim(),
            Line::from(Span::styled(first_line(text), theme::dim())),
        ),
        ChatItem::Tool(tool) => {
            let (marker, head) = tool_head(tool, spinner);
            (marker, Line::from(head))
        }
    };
    let mut block = MessageBlock::new(marker);
    block.push(truncate_line(
        content,
        usize::from(MessageBlock::content_width(width)),
    ));
    vec![block.render(width)]
}

/// 文本首行（空文本为空串，gutter 占位仍由 MessageBlock 渲染）。
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

/// assistant 摘要行：正文首个非空行 > 错误 > 思考占位 > 空消息占位。
fn assistant_summary(assistant: &AssistantItem) -> Line<'static> {
    for block in &assistant.blocks {
        if let Block::Text(text) = block
            && let Some(line) = text.lines().find(|line| !line.trim().is_empty())
        {
            return Line::from(Span::styled(line.to_string(), Style::default()));
        }
    }
    if let Some(error) = &assistant.error {
        return Line::from(Span::styled(format!("✗ {error}"), theme::err()));
    }
    for block in &assistant.blocks {
        if let Block::Thinking(thinking) = block {
            return Line::from(Span::styled(
                format!("思考过程（{} 行）", thinking.lines().count()),
                theme::thinking(),
            ));
        }
    }
    Line::from(Span::styled("…", theme::dim()))
}

/// 把行截断到 `max` 显示宽度：超长时只留前 `max - 1` 列并以 `…` 收尾
///（条目折叠摘要行用；CJK 宽字符按显示宽度计）。
fn truncate_line(line: Line<'static>, max: usize) -> Line<'static> {
    if line.width() <= max {
        return line;
    }
    let limit = max.saturating_sub(1);
    let mut spans = Vec::new();
    let mut used = 0_usize;
    let mut full = false;
    for span in line.spans {
        let mut buf = String::new();
        for c in span.content.chars() {
            let width = UnicodeWidthChar::width(c).unwrap_or(0);
            if used + width > limit {
                full = true;
                break;
            }
            used += width;
            buf.push(c);
        }
        if !buf.is_empty() {
            spans.push(Span::styled(buf, span.style));
        }
        if full {
            break;
        }
    }
    spans.push(Span::styled("…", theme::dim()));
    Line::from(spans)
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
/// 高亮 gutter：续行与高亮空行用加粗竖条，保持区域连续。
const GUTTER_CURSOR_BODY: &str = "▐ ";

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

/// 整行高亮（消息游标）：gutter span 换为指定符号与样式，
/// 行铺暗色背景并补齐到行宽，呈现为整行色带而非参差的 gutter 色点。
/// 空行（块间隔）补 gutter span，保持高亮区竖条连续。
fn restyle_highlight(
    line: Line<'static>,
    marker: Style,
    gutter: &str,
    width: u16,
) -> Line<'static> {
    let bg = theme::highlight_bg();
    let mut spans = line.spans;
    if let Some(first) = spans.first_mut() {
        first.content = gutter.to_string().into();
        first.style = marker;
    } else {
        spans.push(Span::styled(gutter.to_string(), marker));
    }
    // 背景 patch 到每个 span 而非 Line.style：wrap_line / 搜索高亮重建行时
    // 会丢弃行级样式，span 级背景才能存活
    for span in &mut spans {
        span.style = span.style.patch(bg);
    }
    let mut line = Line::from(spans);
    let pad = usize::from(width).saturating_sub(line.width());
    if pad > 0 {
        line.spans.push(Span::styled(" ".repeat(pad), bg));
    }
    line
}

/// 把行内所有大小写不敏感的 `query` 命中片段覆盖为 `hit` 样式
///（搜索高亮）；无命中原样返回。
fn highlight_line(line: &Line<'static>, query: &str, hit: Style) -> Line<'static> {
    let text: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let hits = match_positions(&text, query);
    if hits.iter().all(|hit| !hit) {
        return line.clone();
    }
    // 重建 spans：按命中边界切分，命中片段用 hit 样式、其余保留原样式
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut pos = 0_usize; // 行内字符下标
    for span in &line.spans {
        let mut buf = String::new();
        let mut buf_hit = None;
        for c in span.content.chars() {
            let current_hit = hits.get(pos).copied().unwrap_or(false);
            pos += 1;
            match buf_hit {
                None => {
                    buf_hit = Some(current_hit);
                    buf.push(c);
                }
                Some(previous) if previous == current_hit => buf.push(c),
                Some(previous) => {
                    out.push(Span::styled(
                        std::mem::take(&mut buf),
                        if previous { hit } else { span.style },
                    ));
                    buf.push(c);
                    buf_hit = Some(current_hit);
                }
            }
        }
        if !buf.is_empty() {
            out.push(Span::styled(
                buf,
                if buf_hit == Some(true) {
                    hit
                } else {
                    span.style
                },
            ));
        }
    }
    Line::from(out)
}

/// `query` 在 `text` 中大小写不敏感命中的字符位置表（逐字符 bool）。
fn match_positions(text: &str, query: &str) -> Vec<bool> {
    let mut hits = vec![false; text.chars().count()];
    if query.is_empty() {
        return hits;
    }
    let lower = text.to_lowercase();
    let needle = query.to_lowercase();
    let mut from = 0;
    while let Some(found) = lower.get(from..).and_then(|rest| rest.find(&needle)) {
        let byte_start = from + found;
        let byte_end = byte_start + needle.len();
        let start = lower[..byte_start].chars().count();
        let end = lower[..byte_end].chars().count();
        for hit in &mut hits[start..end] {
            *hit = true;
        }
        from = byte_end;
    }
    hits
}

/// 工具条目标题行（gutter 状态色 + 标题 spans）：完整块与 VISUAL
/// 摘要行共用。运行中带 spinner 帧。
fn tool_head(tool: &ToolItem, spinner: &str) -> (Style, Vec<Span<'static>>) {
    let (mark_style, name_style) = match tool.status {
        ToolStatus::Running => (theme::busy(), theme::bold()),
        ToolStatus::Ok => (theme::ok(), theme::bold()),
        ToolStatus::Failed => (theme::err(), theme::err_bold()),
    };
    let mut spans = Vec::new();
    if tool.status == ToolStatus::Running {
        spans.push(Span::styled(format!("{spinner} "), mark_style));
    }
    spans.push(Span::styled(tool.name.clone(), name_style));
    if !tool.args.is_empty() {
        spans.push(Span::styled(format!("({})", tool.args), theme::dim()));
    }
    (mark_style, spans)
}

/// 工具条目组件：gutter 竖条取状态色，加粗工具名 + 暗色 (参数)，
/// 结果摘要首行 `⎿` 引导、后续行对齐缩进，保持树形层次。
fn tool_block(tool: &ToolItem, spinner: &str) -> MessageBlock {
    let (mark_style, head) = tool_head(tool, spinner);
    let mut block = MessageBlock::new(mark_style);
    block.push(Line::from(head));
    if !tool.detail.is_empty() {
        let detail_style = if tool.status == ToolStatus::Failed {
            theme::err()
        } else {
            theme::dim()
        };
        for (index, detail) in tool.detail.iter().enumerate() {
            let prefix = if index == 0 { "⎿ " } else { "  " };
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
            "Enter 发送（运行中则排队）· Esc 中断/浏览（NORMAL）· Ctrl+G 外部编辑器",
            theme::dim(),
        )),
        Line::from(Span::styled(
            "NORMAL：j/k 滚动 · d/u 半页 · g/G 顶底 · : 命令（/help）· / 搜索 · y 复制 · m 队列 · ? 帮助",
            theme::dim(),
        )),
        Line::from(Span::styled(
            "↑/↓ 历史 · PgUp/PgDn/滚轮滚动 · Ctrl+V 粘贴图片 · Ctrl+C 清草稿/退出",
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

/// 草稿区行数上限：高度随行数伸缩，超过后内部滚动。
const MAX_DRAFT_LINES: u16 = 5;

/// 输入框内容总行数上限（附件行 + 队列区 + 草稿区）：
/// 队列区可见时（ADR-0012）允许比纯草稿更高的伸缩。
const MAX_INPUT_LINES: u16 = 10;

/// 输入框总高度（含上下边框）：附件行（可选）+ 队列区 + 草稿区 + 2 行边框。
/// QUEUE 模式下草稿不单独渲染（就地编辑槽位的行即草稿内容）；
/// COMMAND 模式渲染命令输入框（ADR-0020）而非草稿。
fn input_height(app: &App) -> u16 {
    let draft = if app.queue_mode_active() {
        0
    } else if app.mode() == Mode::Command {
        app.command().line_count().min(MAX_DRAFT_LINES)
    } else {
        app.input().line_count().min(MAX_DRAFT_LINES)
    };
    let content = u16::from(app.input().has_attachments()) + app.queue_display_lines() + draft;
    content.clamp(1, MAX_INPUT_LINES) + 2
}

/// 输入框（多行，高度随行数变化）+ 光标定位。
/// 内容顺序：附件行（可选）→ 队列区（排队消息，ADR-0012）→ 草稿行。
fn draw_input(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let (title, border_style) = input_title(app);
    let mut border = Border::bordered()
        .border_type(BorderType::Plain)
        .border_style(border_style);
    if let Some(title) = title {
        border = border.title(title);
    }
    let inner = border.inner(area);
    // 附件行（可选）在输入文本上方：🖼 文件名列表
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.input().has_attachments() {
        let names = app
            .input()
            .attachment_names()
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(Line::from(Span::styled(
            format!("🖼 {names}"),
            theme::accent(),
        )));
    }
    // SEARCH 下输入框复用为搜索框：显示搜索串而非草稿；COMMAND 下
    // 显示专门的命令输入框（ADR-0020），草稿保留不动
    let searching = app.mode() == Mode::Search;
    let commanding = app.mode() == Mode::Command;
    lines.extend(queue_area_lines(app));
    // 草稿行（QUEUE 模式下不单独渲染；SEARCH 显示搜索串，COMMAND
    // 显示命令输入框）
    if !app.queue_mode_active() {
        let text = if searching {
            app.search().query()
        } else if commanding {
            app.command().text()
        } else {
            app.input().text()
        };
        lines.extend(
            text.split('\n')
                .map(|text| Line::from(Span::raw(text.to_string()))),
        );
    }
    // 行数超过可见高度时滚动到光标所在行
    let attachment_offset = u16::from(app.input().has_attachments());
    let queue_offset = app.queue_display_lines();
    let (cursor_row, cursor_col) = if searching {
        (
            queue_offset,
            u16::try_from(UnicodeWidthStr::width(app.search().query())).unwrap_or(u16::MAX),
        )
    } else if commanding {
        let (row, col) = app.command().cursor_position();
        (queue_offset + row, col)
    } else if app.queue_mode_active() {
        if app.queue().is_editing() {
            // 就地编辑：槽位起始行 + 草稿缓冲内的光标行（gutter 宽 2 列）
            let (row, col) = app.input().cursor_position();
            (app.queue().cursor_row() + row, col.saturating_add(2))
        } else {
            // QUEUE 导航：光标停在游标条目行首（块光标即条目高亮）
            (app.queue().cursor_row(), 0)
        }
    } else {
        let (row, col) = app.input().cursor_position();
        (queue_offset + row, col)
    };
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

/// 队列区内容行（oil.nvim 式缓冲，ADR-0014）：运行中入队的消息（当前
/// 步骤完成后注入本轮）；QUEUE 导航下 gutter 标出游标条目，就地编辑
/// 槽位的内容即草稿缓冲。
fn queue_area_lines(app: &App) -> Vec<Line<'static>> {
    let queue_cursor =
        (app.queue_mode_active() && !app.queue().is_editing()).then(|| app.queue().cursor());
    let editing_slot = app.queue().editing_slot();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, entry) in app.queue().entries().iter().enumerate() {
        if editing_slot == Some(index) {
            for (row, text) in app.input().text().split('\n').enumerate() {
                let gutter = if row == 0 { "❯ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(gutter, theme::accent()),
                    Span::raw(text.to_string()),
                ]));
            }
            continue;
        }
        let cursor = queue_cursor == Some(index);
        let text_style = if cursor {
            theme::accent()
        } else {
            theme::dim()
        };
        for (row, text) in entry.text.split('\n').enumerate() {
            let mut text = text.to_string();
            if row == 0 && entry.images > 0 {
                text = format!("{text}  🖼×{}", entry.images);
            }
            let gutter = match (row, cursor) {
                (0, true) => "❯ ",
                (0, false) => "» ",
                _ => "  ",
            };
            lines.push(Line::from(vec![
                Span::styled(gutter, text_style),
                Span::styled(text, text_style),
            ]));
        }
    }
    lines
}

/// 输入框标题与边框样式：标题只保留运行/picker/搜索/命令行等临时功能态；
/// INSERT/NORMAL/VISUAL 等常驻模式的提示由状态栏徽标与右侧键位提示
/// 承担（ADR-0011），输入框不再叠加，避免同一信息两处渲染。
fn input_title(app: &App) -> (Option<Line<'static>>, Style) {
    // QUEUE 模式（ADR-0012）：队列缓冲标题；运行中叠加 spinner
    if app.queue_mode_active() {
        let mut spans = Vec::new();
        if app.is_running() {
            spans.push(Span::styled(format!("{} ", app.spinner()), theme::busy()));
            spans.push(Span::styled("运行中 · ", theme::busy()));
        }
        let text = if app.queue().is_editing() {
            "队列编辑 · Enter/Esc 保存 · Shift+Enter 换行".to_string()
        } else {
            format!(
                "消息队列 {} 条 · i 编辑 · dd 删除 · J/K 换位 · o 新增 · Esc 返回",
                app.queue().len()
            )
        };
        spans.push(Span::styled(text, theme::accent()));
        return (Some(Line::from(spans)), theme::accent());
    }
    if app.is_running() {
        let mut spans = vec![
            Span::styled(format!("{} ", app.spinner()), theme::busy()),
            Span::styled("运行中 · Ctrl+C 取消", theme::busy()),
        ];
        // 排队消息数（ADR-0014）：运行中 Enter 排队的可见反馈
        if !app.queue().is_empty() {
            spans.push(Span::styled(
                format!(" · {} 条排队（Esc→m 编辑）", app.queue().len()),
                theme::busy(),
            ));
        }
        return (Some(Line::from(spans)), theme::busy());
    }
    if let Some(picker) = app.picker() {
        let title = match picker.kind {
            PickerKind::Resume => "恢复 session · 输入过滤 · ↑/↓ 选择 · Enter 确认 · Esc 取消",
            PickerKind::Tree => "会话树 · 输入过滤 · ↑/↓ 选择 · Enter 创建分支 · Esc 取消",
            PickerKind::Models => "切换模型 · 输入过滤 · ↑/↓ 选择 · Enter 确认 · Esc 取消",
            PickerKind::Reasoning => "思考级别 · ↑/↓ 选择 · Enter 确认 · Esc 取消",
            PickerKind::Session => "会话菜单 · ↑/↓ 选择 · Enter 确认 · Esc 取消",
        };
        (
            Some(Line::from(Span::styled(title, theme::accent()))),
            theme::accent(),
        )
    } else if app.mode() == Mode::Command {
        // COMMAND（ADR-0020）：专门的命令输入框；运行中打开时叠加 spinner
        let mut spans = Vec::new();
        if app.is_running() {
            spans.push(Span::styled(format!("{} ", app.spinner()), theme::busy()));
            spans.push(Span::styled("运行中 · ", theme::busy()));
        }
        spans.push(Span::styled(
            "命令 · Tab 补全 · Enter 执行 · Esc 返回",
            theme::accent(),
        ));
        (Some(Line::from(spans)), theme::accent())
    } else if app.mode() == Mode::Search {
        (
            Some(Line::from(Span::styled(
                format!(
                    "搜索 · Enter 完成 · Esc 取消（{} 处命中）",
                    app.search().match_count()
                ),
                theme::accent(),
            ))),
            theme::accent(),
        )
    } else if app.input().completion().is_some() {
        // 补全弹层自带标题；输入框只以 accent 边框表示补全中
        (None, theme::accent())
    } else if !app.queue().is_empty() {
        // 空闲 + 队列非空 = 异常结束后暂停的排队消息（ADR-0012）
        (
            Some(Line::from(Span::styled(
                format!(
                    "队列暂停 {} 条 · Enter 发送下一条 · Esc→m 编辑",
                    app.queue().len()
                ),
                theme::warn(),
            ))),
            theme::warn(),
        )
    } else if app.mode() == Mode::Normal {
        // NORMAL：输入框不是焦点，降为暗色；草稿保留可见
        (None, theme::dim())
    } else {
        (None, theme::dim())
    }
}

/// 状态栏：左侧模式徽标 + 模型徽标 + 上下文用量 + 告警；
/// 右侧滚动位置 + 随模式切换的键位提示。
fn draw_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    // 模式徽标（ADR-0011）：NORMAL 反色绿块强提示；INSERT/PICKER 低调
    // accent 文本，避免与相邻的模型徽标（反色块）糊成一片
    let mode_badge = match app.mode() {
        Mode::Normal => Span::styled(" NORMAL ", theme::normal_badge()),
        Mode::Command => Span::styled(" COMMAND ", theme::warn()),
        Mode::Search => Span::styled(" SEARCH ", theme::warn()),
        Mode::CopyMenu => Span::styled(" COPY ", theme::queue_badge()),
        Mode::Queue => Span::styled(" QUEUE ", theme::queue_badge()),
        Mode::Insert => Span::styled(" INSERT ", theme::accent()),
        Mode::Picker => Span::styled(" PICKER ", theme::accent()),
        Mode::Help => Span::styled(" HELP ", theme::accent()),
    };
    let mut left = vec![
        mode_badge,
        Span::styled(format!(" {} ", app.model_name()), theme::selected()),
        context_usage_span(app),
    ];
    // goal 模式开启时给出常驻徽标：自动追问进行中用户能看到原因
    if app.goal_mode() {
        left.push(Span::styled(" goal ", theme::warn()));
    }
    if let Some(notice) = app.notice() {
        left.push(Span::styled(format!("⚠ {notice} "), theme::warn()));
    }
    let mut right = Vec::new();
    if app.chat().scroll() > 0 {
        right.push(Span::styled(
            format!("↑ {}/{} ", app.chat().scroll(), app.chat().scroll_max()),
            theme::warn(),
        ));
    }
    // 键位提示保持精简：完整键位见欢迎页与 /help，此处只留模式核心键
    let hint = match app.mode() {
        Mode::Normal => "i 输入 · : 命令 · / 搜索 · ? 帮助 ",
        Mode::Command => "Tab 补全 · Enter 执行 · Esc 返回 ",
        Mode::Search => "输入即搜 · Enter 完成 · Esc 取消 ",
        Mode::CopyMenu => "j/k 选择 · 1-9 直达 · Enter 复制 · Esc 关闭 ",
        Mode::Picker => "输入过滤 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ",
        Mode::Help => "j/k 滚动 · gg/G 顶/底 · Esc 关闭 ",
        Mode::Insert => "Enter 发送 · ^G 编辑器 · Esc 浏览 ",
        Mode::Queue => {
            if app.queue().is_editing() {
                "Enter/Esc 保存 · Shift+Enter 换行 "
            } else {
                "j/k 移动 · i 编辑 · dd 删除 · J/K 换位 · o 新增 "
            }
        }
    };
    right.push(Span::styled(hint, theme::dim()));
    let left_line = Line::from(left);
    let right_line = Line::from(right);
    // 宽度不足时省略右侧提示，避免与左侧信息交叠
    if left_line.width() + right_line.width() <= usize::from(area.width) {
        frame.render_widget(Paragraph::new(right_line).alignment(Alignment::Right), area);
    }
    frame.render_widget(Paragraph::new(left_line), area);
}

/// 状态栏上下文用量：常态紧凑显示占比（`ctx 6%`）；窗口未知（0）时
/// 只显示 token 估算。用量逼近窗口（≥80%）时展开完整数值并以警告色
/// 提示（`ctx 168k/200k·84%`）。
fn context_usage_span(app: &App) -> Span<'static> {
    let tokens = app.context_tokens();
    let window = app.context_window();
    if window == 0 {
        return Span::styled(format!(" ctx {} ", format_tokens(tokens)), theme::dim());
    }
    let percent = tokens.saturating_mul(100) / window;
    if percent >= 80 {
        return Span::styled(
            format!(
                " ctx {}/{}·{}% ",
                format_tokens(tokens),
                format_tokens(window),
                percent
            ),
            theme::warn(),
        );
    }
    Span::styled(format!(" ctx {percent}% "), theme::dim())
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
        .border_type(BorderType::Plain)
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

/// 选择器弹层（`/resume` / `/models` / `/tree` 共用）：与补全弹层同构，贴在输入框上方。
/// 渲染过滤后的可见行；过滤串显示在标题，无匹配时给占位行。
fn draw_picker(frame: &mut Frame<'_>, picker: &Picker, input_area: Rect) {
    let visible = picker.visible();
    let total = visible.len();
    let action = match picker.kind {
        PickerKind::Resume => "恢复 session",
        PickerKind::Tree => "会话树",
        PickerKind::Models => "切换模型",
        PickerKind::Reasoning => "思考级别",
        PickerKind::Session => "会话菜单",
    };
    let mut title = if total > COMPLETION_MAX_VISIBLE {
        format!("{action} {}/{total}", picker.selected + 1)
    } else {
        action.to_string()
    };
    if !picker.filter.is_empty() {
        title = format!("{title} · /{}", picker.filter);
    }
    let lines: Vec<Line<'static>> = if visible.is_empty() {
        vec![Line::from(Span::styled("  无匹配行", theme::dim()))]
    } else {
        let (start, end) = visible_window(total, picker.selected, COMPLETION_MAX_VISIBLE);
        visible[start..end]
            .iter()
            .enumerate()
            .map(|(offset, &row_index)| {
                let row = &picker.rows[row_index];
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
            .collect()
    };
    let block = Border::bordered()
        .border_type(BorderType::Plain)
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

/// 帮助弹层内容（NORMAL `?`）：分组键位表，与 README「TUI 键位」一致。
const HELP_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "通用",
        &[
            ("Esc", "中断运行 / 逐层退回"),
            ("Ctrl+C", "清草稿 → 再按退出"),
            ("Ctrl+D", "草稿为空时退出"),
            ("PgUp/PgDn · 滚轮", "滚动聊天区"),
            ("Shift+拖选", "复制文本（TUI 捕获鼠标）"),
        ],
    ),
    (
        "INSERT（输入）",
        &[
            ("Enter", "发送（运行中排入队列）"),
            ("Shift+Enter · Ctrl+J", "手动换行"),
            ("↑/↓", "输入历史召回"),
            ("Ctrl+W · Ctrl+U", "删词 / 清行"),
            ("Ctrl+A/E · Alt+B/F", "行首行尾 / 词级移动"),
            ("Ctrl+G", "外部编辑器（$VISUAL/$EDITOR）编辑草稿"),
            ("Ctrl+V", "粘贴剪贴板图片"),
            ("Esc", "中断运行（运行中）/ 进入 NORMAL（空闲）"),
        ],
    ),
    (
        "NORMAL（单字母动作层）",
        &[
            ("i a Enter · A · I", "回到输入（光标原位 / 末尾 / 行首）"),
            ("j k · d u · g G", "滚动 / 半页 / 顶部 / 底部（less 式）"),
            ("[ ] · { }", "上/下一条消息 · 上/下一个工具调用"),
            ("/ · n · N", "聊天搜索与跳转"),
            ("y · Y", "复制菜单 / 直接复制最新消息"),
            ("Space", "折叠/展开当前条目"),
            ("m · s · r", "队列编辑 / 会话菜单 / 重试最近一轮"),
            ("e · : · ? · q", "外部编辑器 / 命令 / 帮助 / 退出"),
        ],
    ),
    (
        "复制菜单（y）",
        &[
            ("j k · g G", "选择 / 首 / 尾"),
            ("1-9", "数字键直达复制对应行"),
            ("Enter", "复制选中行并关闭"),
            ("Esc · q", "关闭"),
        ],
    ),
    (
        "COMMAND（:）",
        &[
            ("Enter", "执行命令 / 展开模板（/help 查看全部命令）"),
            ("Tab · ↑/↓", "补全命令 / 模板 / skill 并移动选中"),
            ("Esc", "关补全弹层 / 放弃返回 NORMAL"),
            ("编辑键", "与 INSERT 相同（词级移动、删词等）"),
        ],
    ),
    (
        "QUEUE（m · 队列编辑）",
        &[
            ("j/k · g · G", "移动条目游标 / 队首 / 队尾"),
            (
                "i · o · O · Enter",
                "就地编辑 / 下方 / 上方新增（Enter/Esc 保存）",
            ),
            ("dd · x", "删除条目"),
            ("J · K", "下移 / 上移（换位）"),
            ("Esc", "返回（恢复发送）"),
        ],
    ),
    (
        "SEARCH · PICKER",
        &[
            ("SEARCH", "输入即搜 · Enter 完成 · Esc 取消"),
            ("PICKER", "输入过滤 · ↑/↓ 选择 · Home/End 首尾 · Enter/Esc"),
        ],
    ),
];

/// 键位列的目标显示宽度（键名左对齐，描述另起一栏）。
const HELP_KEY_COL: usize = 26;

/// 帮助弹层的全部内容行（键名列按显示宽度对齐，CJK 友好）。
fn help_lines() -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, (title, rows)) in HELP_GROUPS.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(format!(" {title}"), theme::bold())));
        for (keys, desc) in *rows {
            let pad = HELP_KEY_COL.saturating_sub(UnicodeWidthStr::width(*keys));
            lines.push(Line::from(vec![
                Span::styled(format!("  {keys}{:pad$}", ""), theme::accent()),
                Span::styled((*desc).to_string(), theme::dim()),
            ]));
        }
    }
    lines
}

/// 复制菜单（NORMAL `y`）：模态覆盖层，居中面板列出可复制条目；
/// 选中行高亮，Enter/数字键复制、Esc/q 关闭。
fn draw_copy_menu(frame: &mut Frame<'_>, menu: &CopyMenu, area: Rect) {
    frame.render_widget(Clear, area);
    let rows = menu.rows();
    let selected = menu.selected();
    let lines: Vec<Line<'static>> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let prefix = if index == selected { "▸ " } else { "  " };
            let style = if index == selected {
                theme::selected()
            } else {
                theme::dim()
            };
            Line::from(Span::styled(format!("{prefix}{}", row.label), style))
        })
        .collect();
    let max_width = lines
        .iter()
        .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0);
    let width = max_width
        .saturating_add(3)
        .min(area.width.saturating_sub(2));
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.height.saturating_sub(2));
    let panel = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Border::bordered()
        .border_type(BorderType::Plain)
        .border_style(theme::accent())
        .title(Span::styled(
            "复制 · j/k 选择 · 1-9 直达 · Enter 确认 · Esc 关闭",
            theme::accent(),
        ));
    frame.render_widget(Clear, panel);
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 键位帮助弹层（NORMAL `?`）：模态覆盖层，先清空内容区再在
/// 其中居中面板（避免被覆盖的输入框等留下边框残片）；内容超长时
/// j/k 等滚动。
fn draw_help(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    frame.render_widget(Clear, area);
    let lines = help_lines();
    let max_line_width = lines
        .iter()
        .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0);
    // 宽高取内容与可用区域的较小值，居中；边框 + 左右留白各一列
    let width = max_line_width
        .saturating_add(3)
        .min(area.width.saturating_sub(2));
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.height.saturating_sub(2));
    let panel = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Border::bordered()
        .border_type(BorderType::Plain)
        .border_style(theme::accent())
        .title(Span::styled("键位帮助 · Esc/q/? 关闭", theme::accent()));
    frame.render_widget(Clear, panel);
    let inner = block.inner(panel);
    frame.render_widget(block, panel);
    let max_scroll =
        u16::try_from(lines.len().saturating_sub(usize::from(inner.height))).unwrap_or(u16::MAX);
    let scroll = app.clamp_help_scroll(max_scroll);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

#[cfg(test)]
mod tests {
    use nomic_ai::Message;
    use nomic_core::{AgentEvent, ToolResult};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    /// 搜索高亮：命中片段覆盖 hit 样式（大小写不敏感），跨 span 命中也能切分。
    #[test]
    fn highlight_line_covers_case_insensitive_matches() {
        let line = Line::from(vec![
            Span::styled("Hello ", Style::new()),
            Span::styled("WORLD hello", Style::new()),
        ]);
        let hit = Style::new().fg(ratatui::style::Color::Black);
        let highlighted = highlight_line(&line, "hello", hit);
        let styles: Vec<Style> = highlighted.spans.iter().map(|span| span.style).collect();
        // 「Hello 」命中 + 「WORLD hello」尾部命中：命中片段为 hit，其余原样
        assert_eq!(highlighted.spans[0].content.as_ref(), "Hello");
        assert_eq!(styles[0], hit);
        assert_eq!(
            highlighted.spans.last().expect("last").content.as_ref(),
            "hello"
        );
        assert_eq!(*styles.last().expect("last"), hit);
        assert!(styles.iter().any(|style| *style != hit), "保留未命中片段");

        // 无命中原样返回；空 query 不高亮
        assert_eq!(highlight_line(&line, "xyz", hit).spans.len(), 2);
        assert_eq!(highlight_line(&line, "", hit).spans.len(), 2);
    }

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
        // session id 是内部标识，不对用户展示
        assert!(!compact.contains("abcd1234"));
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

    /// 队列区渲染（ADR-0014）：排队消息显示在输入框草稿上方，运行中
    /// 标题显示条数；QUEUE 模式标题切换、游标条目以 `❯` gutter 标出。
    #[test]
    fn renders_queue_area_and_queue_mode_title() {
        use super::super::app::Key;

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::AgentStart);
        app.paste_text("第一条");
        app.press(Key::Enter);
        app.paste_text("第二条");
        app.press(Key::Enter);

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
        // 运行中标题显示排队条数；条目以 `»` gutter 标出
        assert!(compact.contains("2条排队"), "{compact}");
        assert!(compact.contains("»第一条"), "{compact}");
        assert!(compact.contains("»第二条"), "{compact}");

        // 异常结束后空闲：标题提示队列暂停与恢复方式
        app.finish_run(Some("已取消".to_string()));
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(compact.contains("队列暂停2条"), "{compact}");

        // QUEUE 模式：徽标、标题与游标条目 gutter（❯）
        app.press(Key::Esc);
        app.press(Key::Char('m'));
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(compact.contains("QUEUE"), "{compact}");
        assert!(compact.contains("消息队列2条"), "{compact}");
        assert!(compact.contains("❯第一条"), "{compact}");
        assert!(compact.contains("»第二条"), "{compact}");
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
        assert!(text.contains("⎿ line1"), "{text}");
        assert!(text.contains("  line2"), "{text}");
        assert!(text.contains("  line3"), "{text}");
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
        app.chat_mut().push_system("本地系统提示");
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
    /// 默认折叠（仅占位行），`/thinking` 展开后逐行渲染正文。
    #[test]
    fn renders_thinking_block_with_gutter() {
        let mut app = thinking_app();

        // 默认折叠：不渲染正文行，只渲染一行占位提示
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        assert!(!compact.contains("推理第一行"), "{compact}");
        assert!(!compact.contains("推理第二行"), "{compact}");
        assert!(compact.contains("▌思考过程（2行，已折叠"), "{compact}");

        // `/thinking`（命令行，ADR-0020）展开后渲染正文行
        app.press(super::super::app::Key::Esc);
        app.press(super::super::app::Key::Char(':'));
        app.paste_text("thinking");
        app.press(super::super::app::Key::Enter);
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        assert!(!compact.contains("Thinking"), "{compact}");
        // 命令受理后回 NORMAL，游标落在该消息上：gutter 变为游标高亮竖条
        assert!(compact.contains("▐推理第一行"), "{compact}");
        assert!(compact.contains("▐推理第二行"), "{compact}");
    }

    /// 构造一条含两行 thinking 的 assistant 消息。
    fn thinking_app() -> App {
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
        app
    }

    /// 提取 buffer 全文（去空白），便于断言可见内容。
    fn compact_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect()
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

    /// NORMAL：状态栏显示模式徽标与浏览键位提示，条目行号回写状态层。
    #[test]
    fn renders_normal_mode_badge_and_syncs_item_lines() {
        use super::super::app::Key;
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::User(
            nomic_ai::UserMessage {
                content: nomic_ai::UserMessageContent::Text("你好".to_string()),
                timestamp: 0,
            },
        ))));
        app.press(Key::Esc);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let compact = compact_text(&terminal);
        assert!(compact.contains("NORMAL"), "{compact}");
        // 渲染后条目起始行已回写（游标滚动定位依赖）
        assert_eq!(app.chat_cursor(), Some(0));
    }

    /// NORMAL 消息游标：整行铺暗色背景，gutter 统一为 `▐`（无 `❯` 箭头），
    /// 块间隔空行延续高亮，形成连续的整行色带。
    #[test]
    fn normal_cursor_row_highlight_spans_full_width() {
        use super::super::app::Key;
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::User(
            nomic_ai::UserMessage {
                content: nomic_ai::UserMessageContent::Text("第一行\n第二行".to_string()),
                timestamp: 0,
            },
        ))));
        app.press(Key::Esc);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let cells = buffer.content();
        let position_of = |symbol: &str| -> Option<usize> {
            cells.iter().position(|cell| cell.symbol() == symbol)
        };
        // NORMAL 游标无 `❯` 箭头：gutter 全部为 `▐`
        assert!(position_of("❯").is_none(), "NORMAL 游标不应有 ❯ 箭头");
        // 首行 gutter 为 `▐`：accent 前景 + 行背景（非反色块）
        let head = position_of("▐").expect("游标条目首行 gutter 为 ▐");
        assert_eq!(cells[head].fg, theme::ACCENT);
        assert_eq!(cells[head].bg, theme::ROW_BG);
        // 整行铺背景：聊天区宽度内（不含左右留白列）每个单元格都是行背景；
        // 宽字符的续列被 ratatui 重置为 Reset（渲染随首列样式），跳过
        let head_row = head / width;
        let mut continuation = false;
        for x in 1..(width - 1) {
            if continuation {
                continuation = false;
                continue;
            }
            let cell = &cells[head_row * width + x];
            assert_eq!(cell.bg, theme::ROW_BG, "▐ 行第 {x} 列应为行高亮背景");
            continuation = UnicodeWidthStr::width(cell.symbol()) > 1;
        }
        // 续行 gutter 同为 `▐`：accent 前景，同行背景
        let body = cells[head + width..]
            .iter()
            .position(|cell| cell.symbol() == "▐")
            .map(|offset| head + width + offset)
            .expect("续行 gutter 为 ▐");
        assert_eq!(cells[body].fg, theme::ACCENT);
        assert_eq!(cells[body].bg, theme::ROW_BG);
        // 块间隔空行延续高亮：`▐` 不止两个（首行 + 续行 + 空行）
        assert!(
            cells.iter().filter(|cell| cell.symbol() == "▐").count() >= 3,
            "空行应延续高亮 gutter"
        );
    }

    /// VISUAL 选择区：与游标同族的整行高亮，gutter 为 magenta；
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

    /// 补全弹层（命令行，ADR-0020）与 System 条目也能无 panic 绘制。
    #[test]
    fn renders_completion_popup_and_system_item() {
        use super::super::app::Key;

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.chat_mut().push_system("本地系统提示");
        app.press(Key::Esc);
        app.press(Key::Char(':'));
        app.paste_text("n");
        assert!(app.command().completion().is_some());

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
        app.chat_mut().push_system("第一条");
        app.chat_mut().push_system("第二条");

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

    /// 渲染一帧并提取全部非空白字符，供状态栏断言用。
    fn render_compact(app: &mut App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    /// 状态栏上下文用量：常态只显示紧凑占比；逼近窗口（≥80%）时展开完整数值。
    #[test]
    fn status_bar_shows_context_usage() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.set_context_tokens(8_432);
        let compact = render_compact(&mut app);
        assert!(compact.contains("ctx4%"), "{compact}");
        assert!(!compact.contains("8.4k/200k"), "{compact}");

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.set_context_tokens(168_000);
        let compact = render_compact(&mut app);
        assert!(compact.contains("ctx168k/200k·84%"), "{compact}");
    }

    /// 状态栏模式徽标：INSERT 低调徽标；进入 NORMAL 切换为强提示徽标。
    #[test]
    fn status_bar_badge_follows_mode() {
        use super::super::app::Key;

        // 占位条目避免欢迎页（其键位简介提到 NORMAL）干扰徽标断言
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.chat_mut().push_system("占位");
        let compact = render_compact(&mut app);
        assert!(compact.contains("INSERT"), "{compact}");
        assert!(!compact.contains("NORMAL"), "{compact}");

        let _ = app.press(Key::Esc);
        let compact = render_compact(&mut app);
        assert!(compact.contains("NORMAL"), "{compact}");
        assert!(!compact.contains("INSERT"), "{compact}");

        // NORMAL `:` 进入 COMMAND：徽标与键位提示切换（ADR-0020）
        let _ = app.press(Key::Char(':'));
        let compact = render_compact(&mut app);
        assert!(compact.contains("COMMAND"), "{compact}");
        assert!(!compact.contains("NORMAL"), "{compact}");
    }

    /// 帮助弹层（NORMAL `?`）：渲染分组键位表与 HELP 徽标，Esc 关闭；
    /// 终端高度不足时 G 滚动到底可见末尾分组。
    #[test]
    fn renders_help_overlay() {
        use super::super::app::Key;

        let render_sized = |app: &mut App, width: u16, height: u16| {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal.draw(|frame| draw(frame, app)).expect("draw");
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .flat_map(|cell| cell.symbol().chars())
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.press(Key::Esc);
        app.press(Key::Char('?'));
        // 足够高：全部分组可见
        let compact = render_sized(&mut app, 100, 60);
        assert!(compact.contains("HELP"), "{compact}");
        assert!(compact.contains("键位帮助"), "{compact}");
        assert!(compact.contains("Ctrl+G"), "{compact}");
        assert!(compact.contains("队列编辑"), "{compact}");

        // 高度不足：末尾分组先不可见，G 滚动到底后可见
        let compact = render_sized(&mut app, 100, 20);
        assert!(!compact.contains("队列编辑"), "{compact}");
        app.press(Key::Char('G'));
        let compact = render_sized(&mut app, 100, 20);
        assert!(compact.contains("队列编辑"), "{compact}");

        app.press(Key::Esc);
        let compact = render_sized(&mut app, 100, 20);
        assert!(!compact.contains("HELP"), "{compact}");
    }
}
