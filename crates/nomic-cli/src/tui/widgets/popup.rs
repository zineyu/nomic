//! 弹层 widget：slash/skill 补全弹层与选择器弹层（`/resume`、`/models`、`/tree`）。
//!
//! [`CompletionPopup`] 与 [`PickerPopup`] 是 [`Widget`]：以输入框区域为锚点，
//! 在其顶边向上弹出（先 [`Clear`] 再带边框绘制）。两者同构，仅数据源与
//! 标题不同；选择器弹层的滚动窗口来自选择内核（`crate::picker`，
//! 与 CLI 选择器同一口径），补全弹层因选中循环语义保留本地窗口。

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph, Widget},
};

use crate::tui::app::{Completion, CompletionCandidate, PICKER_ROW_CAPACITY, Picker, PickerKind};
use crate::tui::theme;

/// 补全弹层可见候选数上限，超出时内部滚动窗口。
const COMPLETION_MAX_VISIBLE: usize = 10;

/// 补全弹层可见窗口（居中语义；选中循环，不走选择内核的贴边窗口）：
/// 总数超过上限时让选中项大致居中。
fn visible_window(total: usize, selected: usize, max: usize) -> (usize, usize) {
    if total <= max {
        return (0, total);
    }
    let start = selected.saturating_sub(max / 2).min(total - max);
    (start, start + max)
}

/// slash 命令 / skill 名补全弹层：带边框贴在输入框上方，选中项以 `❯` 前缀标出。
pub(in crate::tui) struct CompletionPopup<'a> {
    completion: &'a Completion,
}

impl<'a> CompletionPopup<'a> {
    pub(in crate::tui) const fn new(completion: &'a Completion) -> Self {
        Self { completion }
    }
}

impl Widget for CompletionPopup<'_> {
    /// `input_area` 为输入框区域：弹层贴其顶边向上弹出，空间不足时压到聊天区顶部。
    fn render(self, input_area: Rect, buf: &mut Buffer) {
        let completion = self.completion;
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
        render_popup(buf, input_area, lines, block);
    }
}

/// 选择器弹层（`/resume` / `/models` / `/tree` 共用）：与补全弹层同构，贴在输入框上方。
/// 渲染过滤后的可见行（滚动窗口取自选择内核）；过滤串显示在标题，无匹配时给占位行。
pub(in crate::tui) struct PickerPopup<'a> {
    picker: &'a Picker,
}

impl<'a> PickerPopup<'a> {
    pub(in crate::tui) const fn new(picker: &'a Picker) -> Self {
        Self { picker }
    }
}

impl Widget for PickerPopup<'_> {
    /// `input_area` 为输入框区域：弹层贴其顶边向上弹出，空间不足时压到聊天区顶部。
    fn render(self, input_area: Rect, buf: &mut Buffer) {
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
        let mut title = if total > PICKER_ROW_CAPACITY {
            format!("{action} {}/{total}", core.selected + 1)
        } else {
            action.to_string()
        };
        if !core.filter.is_empty() {
            title = format!("{title} · /{}", core.filter);
        }
        let lines: Vec<Line<'static>> = if visible.is_empty() {
            vec![Line::from(Span::styled("  无匹配行", theme::dim()))]
        } else {
            let start = core.window(PICKER_ROW_CAPACITY);
            let end = (start + PICKER_ROW_CAPACITY).min(total);
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
        render_popup(buf, input_area, lines, block);
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
    use super::*;

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
}
