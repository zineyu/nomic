//! 聊天区 widget：历史条目 + 流式累积，软换行与精确滚动。
//!
//! [`ChatView`] 是纯只读 [`Widget`]：行组装与折行复用
//! [`crate::tui::chat_lines`]（状态层几何与上屏同一实现），条目起始行与
//! 滚动上限在渲染前已由 [`App::sync_chat_geometry`] 按本区域宽高算进
//! [`Chat`](crate::tui::app::Chat)——渲染期不回写状态，「先渲一帧才有几何」
//! 的时序依赖随之消除，折行实现可在 [`crate::tui::chat_lines`] 一处替换
//! 而不惊动状态层。

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::tui::app::App;
use crate::tui::chat_lines::chat_lines;
use crate::tui::theme;
use crate::tui::widgets::message::wrap_lines;

/// 聊天区左右留白列数，避免输出紧贴屏幕边缘。
pub(in crate::tui) const CHAT_H_MARGIN: u16 = 1;

/// 聊天区 widget：只读渲染 [`App`]（几何已在渲染前算进状态层）。
pub(in crate::tui) struct ChatView<'a> {
    app: &'a App,
}

impl<'a> ChatView<'a> {
    pub(in crate::tui) const fn new(app: &'a App) -> Self {
        Self { app }
    }
}

impl Widget for ChatView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let chat = self.app.chat();
        if chat.items().is_empty() {
            render_welcome(buf, area, self.app.model_name());
            return;
        }
        // 与状态层几何同一行组装（游标高亮只补样式不改行数）；
        // 自行折行（硬换行，CJK 友好），行数与几何精确一致
        let (lines, _) = chat_lines(
            chat.items(),
            area.width,
            self.app.chat_cursor(),
            self.app.thinking_collapsed(),
            self.app.spinner(),
        );
        let lines = wrap_lines(&lines, area.width);
        // 搜索命中高亮：Enter 后保留（Esc 清空搜索串即消除）
        let lines = if let Some(query) = self.app.search().highlight() {
            lines
                .iter()
                .map(|line| highlight_line(line, query, theme::search_hit()))
                .collect()
        } else {
            lines
        };
        // 滚动偏移与上限读状态层（渲染前已按本区域宽高同步并钳制）
        let offset = chat.scroll_max().saturating_sub(chat.scroll());
        Paragraph::new(lines).scroll((offset, 0)).render(area, buf);
    }
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
