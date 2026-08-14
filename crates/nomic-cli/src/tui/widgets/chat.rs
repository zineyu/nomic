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
        // 与状态层几何同一行组装；自行折行（硬换行，CJK 友好），
        // 行数与几何精确一致
        let lines = chat_lines(
            chat.items(),
            area.width,
            self.app.thinking_collapsed(),
            self.app.spinner(),
        );
        let lines = wrap_lines(&lines, area.width);
        // 滚动偏移与上限读状态层（渲染前已按本区域宽高同步并钳制）
        let offset = chat.scroll_max().saturating_sub(chat.scroll());
        Paragraph::new(lines).scroll((offset, 0)).render(area, buf);
    }
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
            "NORMAL：j/k 滚动 · d/u 半页 · g/G 顶底 · : 命令（/help）· Y 复制最新 · m 队列 · ? 帮助",
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
