//! 聊天区行组装：条目 → 带 gutter 的物理行 + 各条目起始行（几何）。
//!
//! 这是「宽度 → 折行 → 条目起始行」的唯一实现：状态层
//!（[`Chat::sync_geometry`](crate::tui::app::Chat)）渲染前按已知视口
//! 主动计算几何，渲染 widget（[`ChatView`](crate::tui::widgets)）用同一
//! 函数上屏——行数与起始行精确一致，滚动偏移才精确。游标整行高亮只
//! 改样式与补齐行宽，不改行数，几何计算传 `cursor: None` 即可。

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::tui::app::{AssistantItem, Block, ChatItem, ToolItem, ToolStatus};
use crate::tui::markdown;
use crate::tui::theme;
use crate::tui::widgets::message::{GUTTER_CURSOR_BODY, MessageBlock, truncate_line};

/// 组装聊天区逻辑行：完整消息块 + 块间空行；游标条目整行高亮
///（gutter 换符号与样式、行铺背景并补齐行宽）。
/// 返回（逻辑行，各条目起始行——状态层几何与消息游标滚动定位用）。
pub(in crate::tui) fn chat_lines(
    items: &[ChatItem],
    width: u16,
    cursor: Option<usize>,
    thinking_collapsed: bool,
    spinner: &str,
) -> (Vec<Line<'static>>, Vec<u16>) {
    // 每个块标注所属条目下标：游标 gutter 高亮与条目起始行计算用
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
/// 只改样式与补齐行宽，不改行数——几何计算（`cursor: None`）与上屏一致。
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
