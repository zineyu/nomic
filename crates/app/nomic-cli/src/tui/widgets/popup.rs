//! 弹层 widget：`@` mention 补全弹层与选择器弹层（`resume`、`models`、
//! `tree`、思考级别命令）。
//!
//! [`MentionPopup`] 以输入框区域为锚点，在其顶边向上弹出（先 [`Clear`]
//! 再带边框绘制）——mention 补全服务于草稿输入框的 `@` 标记，贴框展示
//! 语义自然。选择器弹层 [`PickerPopup`] 是模态浮层：与帮助/提问弹层
//! 同构，以内容区（状态栏以上）整体作为画布，先 [`Clear`] 再居中面板
//!（ADR-0020 修订后命令面收敛到浮层，选择器同样不再贴输入框）。
//!
//! 选择器弹层的滚动窗口来自选择内核（`crate::picker`，与 CLI 选择器
//! 同一口径），mention 弹层因选中循环语义保留本地窗口。
//! 命令补全不走弹层：浮层命令栏（[`super::palette`]）自带候选列表。

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::{MentionCompletion, PICKER_ROW_CAPACITY, Picker, PickerKind};
use crate::tui::theme;

use super::overlay::centered_panel;

/// 弹层可见候选数上限，超出时内部滚动窗口。
const POPUP_MAX_VISIBLE: usize = 10;

/// 弹层可见窗口（居中语义；选中循环，不走选择内核的贴边窗口）：
/// 总数超过上限时让选中项大致居中。浮层命令栏的候选列表与 mention
/// 弹层共用。
pub(in crate::tui) fn visible_window(total: usize, selected: usize, max: usize) -> (usize, usize) {
    if total <= max {
        return (0, total);
    }
    let start = selected.saturating_sub(max / 2).min(total - max);
    (start, start + max)
}

/// `@` mention 补全弹层：贴在输入框上方。
/// 数据源是草稿输入框的 mention 候选（skill 名 / 文件路径 / 类型提示）。
pub(in crate::tui) struct MentionPopup<'a> {
    mention: &'a MentionCompletion,
}

impl<'a> MentionPopup<'a> {
    pub(in crate::tui) const fn new(mention: &'a MentionCompletion) -> Self {
        Self { mention }
    }
}

impl Widget for MentionPopup<'_> {
    fn render(self, input_area: Rect, buf: &mut Buffer) {
        let mention = self.mention;
        let total = mention.candidates.len();
        let (start, end) = visible_window(total, mention.selected, POPUP_MAX_VISIBLE);
        let lines: Vec<Line<'static>> = mention.candidates[start..end]
            .iter()
            .enumerate()
            .map(|(offset, candidate)| {
                if start + offset == mention.selected {
                    Line::from(vec![
                        Span::styled("❯ ", theme::user_marker()),
                        Span::styled(candidate.display.clone(), theme::accent()),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(candidate.display.clone(), theme::subtle()),
                    ])
                }
            })
            .collect();
        let title = if total > POPUP_MAX_VISIBLE {
            format!("提及 {}/{total}", mention.selected + 1)
        } else {
            "提及".to_string()
        };
        let block = Border::bordered()
            .border_type(BorderType::Plain)
            .border_style(theme::accent())
            .title(Span::styled(title, theme::accent()));
        render_popup(buf, input_area, lines, block);
    }
}

/// 选择器弹层（`resume` / `models` / `tree` / 思考级别命令共用）：模态
/// 浮层——与帮助/提问弹层同构，内容区（状态栏以上）整体作为画布，先
/// [`Clear`] 再居中面板。首行是过滤输入行（`/` 提示符 + 过滤串，与
/// 浮层命令栏的 `:` 提示符同构，终端光标落在行内），下方为过滤后的
/// 可见行，末行为键位提示。
pub(in crate::tui) struct PickerPopup<'a> {
    picker: &'a Picker,
}

impl<'a> PickerPopup<'a> {
    pub(in crate::tui) const fn new(picker: &'a Picker) -> Self {
        Self { picker }
    }

    /// 过滤输入行的终端光标位置（渲染后由组合根设置）：定位在 `/` 提示
    /// 符后的过滤串文本处；长过滤串贴面板右边界截断（不横向滚动）。
    pub(in crate::tui) fn cursor_position(&self, area: Rect) -> Position {
        let (lines, _) = self.content();
        let inner = Border::bordered().inner(centered_panel(area, &lines));
        let filter_width = UnicodeWidthStr::width(self.picker.core.filter.as_str());
        let col = 1u16
            .saturating_add(u16::try_from(filter_width).unwrap_or(u16::MAX))
            .min(inner.width.saturating_sub(1));
        Position::new(inner.x + col, inner.y)
    }

    /// 面板内容行与标题（渲染与光标定位共用，保证几何一致）：
    /// 过滤输入行 + 可见行（或「无匹配行」占位）+ 键位提示。
    fn content(&self) -> (Vec<Line<'static>>, String) {
        let picker = self.picker;
        let core = &picker.core;
        let visible = picker.visible();
        let total = visible.len();
        let action = match picker.kind {
            PickerKind::Resume => "恢复 session",
            PickerKind::Tree => "会话树",
            PickerKind::Models => "切换模型",
            PickerKind::Reasoning => "思考级别",
        };
        let title = if total > PICKER_ROW_CAPACITY {
            format!("{action} {}/{total}", core.selected + 1)
        } else {
            action.to_string()
        };
        let mut lines = vec![Line::from(vec![
            Span::styled("/", theme::accent()),
            Span::raw(core.filter.clone()),
        ])];
        if visible.is_empty() {
            lines.push(Line::from(Span::styled("  无匹配行", theme::dim())));
        } else {
            let start = core.window(PICKER_ROW_CAPACITY);
            let end = (start + PICKER_ROW_CAPACITY).min(total);
            lines.extend(
                visible[start..end]
                    .iter()
                    .enumerate()
                    .map(|(offset, &row_index)| {
                        let row = &core.rows[row_index];
                        if start + offset == core.selected {
                            Line::from(vec![
                                Span::styled("❯ ", theme::user_marker()),
                                Span::styled(row.text.clone(), theme::accent()),
                            ])
                        } else {
                            // 不可选行（`tree` 的工具调用条目）再降一档，仅作浏览上下文
                            let style = if row.selectable {
                                theme::subtle()
                            } else {
                                theme::dim()
                            };
                            Line::from(vec![Span::raw("  "), Span::styled(row.text.clone(), style)])
                        }
                    }),
            );
        }
        lines.push(Line::from(Span::styled(
            picker_hint(picker.kind),
            theme::dim(),
        )));
        (lines, title)
    }
}

impl Widget for PickerPopup<'_> {
    /// `area` 为内容区画布：先整体 [`Clear`]，再居中面板绘制。
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (lines, title) = self.content();
        let panel = centered_panel(area, &lines);
        let block = Border::bordered()
            .border_type(BorderType::Plain)
            .border_style(theme::accent())
            .title(Span::styled(title, theme::accent()));
        Clear.render(panel, buf);
        let inner = block.inner(panel);
        block.render(panel, buf);
        Paragraph::new(lines).render(inner, buf);
    }
}

/// 选择器底部键位提示（随种类变化；原输入框标题的提示移入弹层）。
const fn picker_hint(kind: PickerKind) -> &'static str {
    match kind {
        PickerKind::Resume => "输入即过滤 · ↑/↓ 选择 · Enter 恢复 · Esc 取消",
        PickerKind::Tree => "输入即过滤 · ↑/↓ 选择 · Enter 创建分支 · Esc 取消",
        PickerKind::Models => "输入即过滤 · ↑/↓ 选择 · Enter 切换 · Esc 取消",
        PickerKind::Reasoning => "输入即过滤 · ↑/↓ 选择 · Enter 确认 · Esc 取消",
    }
}

/// 弹层共用渲染：宽度对齐最长行 + 边框与右留白，贴输入框顶边向上弹出；
/// 先 [`Clear`] 再带边框绘制。
fn render_popup(buf: &mut Buffer, input_area: Rect, lines: Vec<Line<'static>>, block: Border<'_>) {
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
    Clear.render(area, buf);
    Paragraph::new(lines).block(block).render(area, buf);
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    use super::*;
    use crate::tui::app::{App, Key, PickerRow};
    use crate::tui::widgets::draw;

    /// 弹层滚动窗口：不超限全量显示；超限时选中项保持在窗内。
    #[test]
    fn visible_window_keeps_selected_visible() {
        assert_eq!(visible_window(5, 3, 10), (0, 5));
        assert_eq!(visible_window(20, 0, 10), (0, 10));
        let (start, end) = visible_window(20, 12, 10);
        assert!(start <= 12 && 12 < end);
        assert_eq!(end - start, 10);
        // 末尾选中贴底
        assert_eq!(visible_window(20, 19, 10), (10, 20));
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

    fn render(app: &mut App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        compact_text(&terminal)
    }

    /// `resume` 选择器弹层（浮层）：标题、过滤输入行与 session 行均可见。
    #[test]
    fn renders_resume_picker() {
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
        let compact = render(&mut app);
        assert!(compact.contains("恢复session"), "{compact}");
        assert!(compact.contains("01999999"), "{compact}");
        assert!(compact.contains("02888888"), "{compact}");
    }

    /// `models` 选择器弹层（浮层，ADR-0020 修订）：标题、过滤输入行与
    /// 模型行均可见，预选中当前模型；键位提示收在弹层内，输入框不再叠加
    /// 选择器标题。
    #[test]
    fn renders_model_picker() {
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
        let compact = render(&mut app);
        assert!(compact.contains("切换模型"), "{compact}");
        assert!(compact.contains("claude-sonnet-4-5"), "{compact}");
        assert!(compact.contains("claude-opus-4-7"), "{compact}");
        // 键位提示收在浮层弹层；旧的输入框标题（输入过滤…）不再叠加
        assert!(compact.contains("输入即过滤"), "{compact}");
        assert!(!compact.contains("输入过滤"), "{compact}");

        // 过滤输入行：可打印字符即过滤，`/` 提示符 + 过滤串显示在弹层内
        app.press(Key::Char('c'));
        let compact = render(&mut app);
        assert!(compact.contains("/c"), "{compact}");
    }

    /// 选择器弹层光标（浮层，ADR-0020 修订）：落在弹层内的过滤输入行
    /// （`/` 提示符后），不在输入框；过滤串增长时光标右移、行不变。
    #[test]
    fn picker_cursor_lands_in_filter_row() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.open_model_picker(
            vec![PickerRow {
                selectable: true,
                id: "m1".to_string(),
                text: "model one".to_string(),
            }],
            0,
        );
        let content = Rect {
            height: 23,
            ..Rect::new(0, 0, 80, 24)
        };
        let pos = PickerPopup::new(app.picker().expect("picker")).cursor_position(content);
        // 光标在弹层内：内容区之内且远在输入框（底部约 3 行）之上
        assert!(pos.x >= 1 && pos.x < content.width, "{pos:?}");
        assert!(pos.y < content.height.saturating_sub(5), "{pos:?}");

        // 过滤串增长：光标右移一位，仍在同一行
        app.press(Key::Char('x'));
        let next = PickerPopup::new(app.picker().expect("picker")).cursor_position(content);
        assert_eq!(next.x, pos.x + 1, "{next:?}");
        assert_eq!(next.y, pos.y, "{next:?}");
    }
}
