//! 输入区 widget：多行草稿输入框 + 附件行 + 队列区 + 光标定位。
//!
//! [`InputArea`] 是 [`Widget`]：从 [`App`] 只读构建输入框画面（附件行 →
//! 队列区 → 草稿），高度随内容伸缩；光标位置在渲染后由组合根经
//! [`InputArea::cursor_position`] 计算并设置（[`Widget::render`]
//! 只拿到 `&mut Buffer`，光标设置属于 `Frame` 职责）。
//! 命令不经过这里：COMMAND 模式的浮层命令栏见 [`super::palette`]。

use crate::tui::app::{App, Mode};
use crate::tui::theme;
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Paragraph, Widget},
};

/// 草稿区行数上限：高度随行数伸缩，超过后内部滚动。
const MAX_DRAFT_LINES: u16 = 5;

/// 输入框内容总行数上限（附件行 + 队列区 + 草稿区）：
/// 队列区可见时（ADR-0012）允许比纯草稿更高的伸缩。
const MAX_INPUT_LINES: u16 = 10;

/// 输入框总高度（含上下边框）：附件行（可选）+ 队列区 + 草稿区 + 2 行边框。
/// QUEUE 模式下草稿不单独渲染（就地编辑槽位的行即草稿内容）；
/// COMMAND 模式下草稿照常渲染（命令在浮层命令栏，不复用此区域）。
pub(in crate::tui) fn input_height(app: &App) -> u16 {
    let draft = if app.queue_mode_active() {
        0
    } else {
        app.input().line_count().min(MAX_DRAFT_LINES)
    };
    let content = u16::from(app.input().has_attachments()) + app.queue_display_lines() + draft;
    content.clamp(1, MAX_INPUT_LINES) + 2
}

/// 输入区 widget：从 [`App`] 只读构建画面。
pub(in crate::tui) struct InputArea<'a> {
    app: &'a App,
}

impl<'a> InputArea<'a> {
    pub(in crate::tui) const fn new(app: &'a App) -> Self {
        Self { app }
    }

    /// 光标定位（渲染后由组合根设置）：定位在文本处；长行贴右边界截断
    ///（不横向滚动）。
    pub(in crate::tui) fn cursor_position(&self, area: Rect) -> Position {
        let metrics = self.metrics(area);
        let visible = metrics.inner.height.max(1);
        let x = metrics.inner.x
            + metrics
                .cursor_col
                .min(metrics.inner.width.saturating_sub(1));
        let y = metrics.inner.y + (metrics.cursor_row - metrics.scroll).min(visible - 1);
        Position::new(x, y)
    }

    /// 渲染度量：内框区域、滚动偏移与光标（逻辑行, 行内显示列）。
    /// 渲染与光标定位共用，保证两处滚动口径一致。
    fn metrics(&self, area: Rect) -> Metrics {
        let app = self.app;
        let inner = Border::bordered()
            .border_type(BorderType::Plain)
            .inner(area);
        let attachment_offset = u16::from(app.input().has_attachments());
        let queue_offset = app.queue_display_lines();
        let (cursor_row, cursor_col) = if app.queue_mode_active() {
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
        // 行数超过可见高度时滚动到光标所在行
        let scroll = cursor_row.saturating_sub(visible - 1);
        Metrics {
            inner,
            scroll,
            cursor_row,
            cursor_col,
        }
    }
}

/// 渲染度量（[`InputArea::metrics`] 的返回）。
struct Metrics {
    inner: Rect,
    scroll: u16,
    cursor_row: u16,
    cursor_col: u16,
}

impl Widget for InputArea<'_> {
    /// 渲染输入框。内容顺序：附件行（可选）→ 队列区（排队消息，ADR-0012）→ 草稿行。
    fn render(self, area: Rect, buf: &mut Buffer) {
        let app = self.app;
        let (title, border_style) = input_title(app);
        let mut border = Border::bordered()
            .border_type(BorderType::Plain)
            .border_style(border_style);
        if let Some(title) = title {
            border = border.title(title);
        }
        let metrics = self.metrics(area);
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
        // COMMAND 下草稿照常渲染（焦点在浮层命令栏），草稿保留不动
        lines.extend(queue_area_lines(app));
        // 草稿行（QUEUE 模式下不单独渲染）
        if !app.queue_mode_active() {
            let text = app.input().text();
            lines.extend(
                text.split('\n')
                    .map(|text| Line::from(Span::raw(text.to_string()))),
            );
        }
        Paragraph::new(lines)
            .block(border)
            .scroll((metrics.scroll, 0))
            .render(area, buf);
    }
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

/// 输入框标题与边框样式：标题只保留运行/队列等临时功能态；
/// INSERT/NORMAL 等常驻模式的提示由状态栏徽标与右侧键位提示
/// 承担（ADR-0011），输入框不再叠加，避免同一信息两处渲染。
/// 选择器打开时输入框失焦：说明与键位提示在浮层弹层自身。
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
            Span::styled("运行中 · NORMAL q 中断/退出", theme::busy()),
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
    // 选择器打开时输入框失焦：不叠加标题（选择器的说明与键位提示
    // 已收进浮层弹层自身），边框降为暗色
    if app.picker().is_some() {
        return (None, theme::dim());
    }
    if app.input().completion().is_some() {
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
