//! 消息块组件与行折行工具：聊天区所有条目共享的视觉单元与折行实现。
//!
//! [`MessageBlock`] 把一条消息渲染为带 gutter 竖条的物理行组（gutter
//! 颜色区分条目类型），并负责按显示宽度折行（CJK 友好，续行延续竖条）。
//! 折行基于 `unicode_width`，使行数精确可知以支撑聊天区的精确滚动；
//! [`wrap_line`] / [`wrap_lines`] 供组件外的裸行（条目留白等）折行。

use ratatui::{
    style::Style,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

/// gutter 竖条前缀：每条物理行的行首。
pub(in crate::tui) const GUTTER_PREFIX: &str = "▌ ";
/// gutter 占用列数：`▌` + 空格。
pub(in crate::tui) const GUTTER_WIDTH: u16 = 2;

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
pub(in crate::tui) struct MessageBlock {
    /// gutter 竖条样式（颜色区分条目类型）。
    marker: Style,
    /// 正文逻辑行（未加竖条、未折行）。
    lines: Vec<Line<'static>>,
}

impl MessageBlock {
    pub(in crate::tui) const fn new(marker: Style) -> Self {
        Self {
            marker,
            lines: Vec::new(),
        }
    }

    /// 追加一行正文（逻辑行，折行由组件负责）。
    pub(in crate::tui) fn push(&mut self, line: Line<'static>) {
        self.lines.push(line);
    }

    /// 正文可用宽度：总宽减去 gutter 两列。
    pub(in crate::tui) fn content_width(width: u16) -> u16 {
        width.saturating_sub(GUTTER_WIDTH).max(1)
    }

    /// 渲染为物理行：每行加 gutter 竖条并按显示宽度折行，续行延续竖条。
    pub(in crate::tui) fn render(&self, width: u16) -> Vec<Line<'static>> {
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

/// 把单个逻辑行按显示宽度折成物理行（保留 span 样式）。
pub(in crate::tui) fn wrap_line(line: &Line<'static>, max: usize) -> Vec<Line<'static>> {
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
pub(in crate::tui) fn wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let max = usize::from(width).max(1);
    lines.iter().flat_map(|line| wrap_line(line, max)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme;

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

    /// 裸行折行：无 gutter 前缀，行为不变。
    #[test]
    fn wrapped_plain_lines_have_no_gutter() {
        let line = Line::from(Span::raw("abcdefgh"));
        let wrapped = wrap_lines(&[line], 4);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].spans[0].content.as_ref(), "abcd");
        assert_eq!(wrapped[1].spans[0].content.as_ref(), "efgh");
    }
}
