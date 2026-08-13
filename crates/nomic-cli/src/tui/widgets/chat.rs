//! 聊天区 widget：历史条目 + 流式累积，软换行与精确滚动。
//!
//! [`ChatView`] 是 [`StatefulWidget`]：状态为聊天区的 [`Chat`]（条目 +
//! 滚动），渲染时把折行后的滚动边界与各条目起始行回写状态层
//!（[`Chat::clamp_scroll`] / [`Chat::sync_item_lines`]），这是状态层与
//! 渲染层唯一的回写通道——自行折行使行数精确可知，滚动偏移才精确。

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, StatefulWidget, Widget},
};

use crate::tui::app::{App, AssistantItem, Block, Chat, ChatItem, ToolItem, ToolStatus};
use crate::tui::markdown;
use crate::tui::theme;
use crate::tui::widgets::message::{GUTTER_CURSOR_BODY, MessageBlock, truncate_line, wrap_lines};

/// 聊天区左右留白列数，避免输出紧贴屏幕边缘。
pub(in crate::tui) const CHAT_H_MARGIN: u16 = 1;

/// 聊天区 widget：从只读渲染参数 + 聊天区状态构建画面。
///
/// 只读参数（模型名、spinner、搜索高亮词、thinking 折叠、消息游标）在
/// 构造时从 [`App`] 取出（owned，不持借用），可变的滚动回写经
/// [`StatefulWidget::State`]（`&mut Chat`）完成。
pub(in crate::tui) struct ChatView {
    /// 模型展示名（空状态欢迎页用）。
    model_name: String,
    /// spinner 帧字符（运行中工具与流式指示共用）。
    spinner: &'static str,
    /// 搜索高亮词（Enter 后保留；`None` 不高亮）。
    search_query: Option<String>,
    /// thinking 内容是否折叠显示（`/thinking` 切换）。
    thinking_collapsed: bool,
    /// 消息游标（浏览类模式下整行高亮的目标条目下标）。
    cursor: Option<usize>,
}

impl ChatView {
    /// 从应用状态收集只读渲染参数（聊天区状态由 `State` 提供）。
    pub(in crate::tui) fn new(app: &App) -> Self {
        Self {
            model_name: app.model_name().to_string(),
            spinner: app.spinner(),
            search_query: app.search().highlight().map(str::to_string),
            thinking_collapsed: app.thinking_collapsed(),
            cursor: app.chat_cursor(),
        }
    }
}

impl StatefulWidget for ChatView {
    type State = Chat;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if state.items().is_empty() {
            state.clamp_scroll(0);
            render_welcome(buf, area, &self.model_name);
            return;
        }
        let (lines, starts) = chat_lines(
            state.items(),
            area.width,
            self.cursor,
            self.thinking_collapsed,
            self.spinner,
        );
        // 自行折行（硬换行，CJK 友好），使行数精确可知、滚动偏移精确
        let lines = wrap_lines(&lines, area.width);
        // 搜索命中高亮：Enter 后保留（Esc 清空搜索串即消除）
        let lines = if let Some(query) = &self.search_query {
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
        let scroll = state.clamp_scroll(max_scroll);
        state.sync_item_lines(starts);
        let offset = max_scroll.saturating_sub(scroll);
        Paragraph::new(lines).scroll((offset, 0)).render(area, buf);
    }
}

/// 组装聊天区逻辑行：完整消息块 + 块间空行；游标条目整行高亮
///（gutter 换符号与样式、行铺背景并补齐行宽）。
/// 返回（逻辑行，各条目起始行——消息游标滚动定位用，回写状态层）。
fn chat_lines(
    items: &[ChatItem],
    width: u16,
    cursor: Option<usize>,
    thinking_collapsed: bool,
    spinner: &str,
) -> (Vec<Line<'static>>, Vec<u16>) {
    // 每个块标注所属条目下标：游标 gutter 高亮与条目起始行回写用
    let mut blocks: Vec<(usize, Vec<Line<'static>>)> = Vec::new();
    for (index, item) in items.iter().enumerate() {
        for block in item_blocks(item, width, thinking_collapsed, spinner) {
            blocks.push((index, block));
        }
    }
    // 拼接：每个消息块后空一行，块间分隔与末尾留白（与输入框拉开距离）统一处理；
    // 同时记录各条目起始行。块间隔空行同样延续高亮，形成连续的高亮区。
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut starts = vec![u16::MAX; items.len()];
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
pub(in crate::tui) fn highlight_line(
    line: &Line<'static>,
    query: &str,
    hit: Style,
) -> Line<'static> {
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
pub(in crate::tui) fn match_positions(text: &str, query: &str) -> Vec<bool> {
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
fn render_welcome(buf: &mut Buffer, area: Rect, model_name: &str) {
    let lines = vec![
        Line::from(Span::styled(
            format!("▌ nomic v{}", env!("CARGO_PKG_VERSION")),
            theme::user_marker(),
        )),
        Line::from(Span::styled(
            format!("agent TUI · {model_name}"),
            theme::dim(),
        )),
        Line::default(),
        Line::from(Span::styled(
            "Enter 发送（运行中则排队）· Esc 浏览（NORMAL；NORMAL 下再按 Esc 中断）· Ctrl+G 外部编辑器",
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
    Paragraph::new(lines)
        .alignment(Alignment::Center)
        .render(centered, buf);
}

#[cfg(test)]
mod tests {
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
}
