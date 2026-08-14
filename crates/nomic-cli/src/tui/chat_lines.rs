//! 聊天区行组装：条目 → 带 gutter 的物理行。
//!
//! 这是「宽度 → 折行」的唯一实现：状态层
//!（[`Chat::sync_geometry`](crate::tui::app::Chat)）渲染前按已知视口
//! 主动计算几何，渲染 widget（[`ChatView`](crate::tui::widgets)）用同一
//! 函数上屏——行数精确一致，滚动偏移才精确。

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::tui::app::{Block, ChatItem, ToolItem, ToolStatus};
use crate::tui::markdown;
use crate::tui::theme;
use crate::tui::widgets::message::MessageBlock;

/// 组装聊天区逻辑行：完整消息块 + 块间空行（块间分隔与末尾留白——
/// 与输入框拉开距离——统一处理）。
pub(in crate::tui) fn chat_lines(
    items: &[ChatItem],
    width: u16,
    thinking_collapsed: bool,
    spinner: &str,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for item in items {
        for block in item_blocks(item, width, thinking_collapsed, spinner) {
            lines.extend(block);
            lines.push(Line::default());
        }
    }
    lines
}

/// 单个聊天条目渲染为消息块列表（每块是一组带 gutter 的物理行）；
/// 运行中工具经 `spinner` 传帧。
fn item_blocks(
    item: &ChatItem,
    width: u16,
    thinking_collapsed: bool,
    spinner: &str,
) -> Vec<Vec<Line<'static>>> {
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
                        // 折叠时只渲染一行占位（`thinking` 命令切换）
                        let mut message = MessageBlock::new(theme::thinking_marker());
                        if thinking_collapsed {
                            let count = thinking.lines().count();
                            message.push(Line::from(Span::styled(
                                format!("思考过程（{count} 行，已折叠；thinking 展开）"),
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

/// 工具条目标题行（gutter 状态色 + 标题 spans）：运行中带 spinner 帧。
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
