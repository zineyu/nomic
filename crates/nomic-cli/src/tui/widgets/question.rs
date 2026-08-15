//! 提问弹层 widget（`ask_user_question` 工具，ADR-0029）：模态覆盖层。
//!
//! 与帮助弹层同构：内容区（状态栏以上）整体作为画布，先 [`Clear`] 再
//! 在其中居中面板。选项列表带类型标记（单选 `○/●`、多选 `☐/☑`）与
//! 游标 `❯`；自定义输入阶段把光标 `▌` 画在输入行内（终端光标位置由
//! [`QuestionOverlay::cursor_position`] 交给 draw 同步设置）。

use nomic_tools::{CUSTOM_OPTION, QuestionKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph, Widget},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::app::Question;
use crate::tui::theme;
use crate::tui::widgets::overlay::centered_panel;

/// 问题文本的折行上限（面板宽度据此收敛，长问题不撑爆屏幕）。
const MAX_LINE_WIDTH: usize = 56;
/// 自定义输入行的前缀（光标定位与渲染共用的宽度基准）。
const CUSTOM_PREFIX: &str = "✏️ 自定义：";

/// 提问弹层：只读构建自 [`Question`] 状态。
pub(in crate::tui) struct QuestionOverlay<'a> {
    question: &'a Question,
}

impl<'a> QuestionOverlay<'a> {
    pub(in crate::tui) const fn new(question: &'a Question) -> Self {
        Self { question }
    }

    /// 自定义输入阶段的终端光标位置（面板内输入行）；列表阶段无键入，
    /// 返回 `None`（光标形状由 [`super::super::terminal::block_cursor`] 管）。
    pub(in crate::tui) fn cursor_position(&self, area: Rect) -> Option<(u16, u16)> {
        if !self.question.is_custom_input() {
            return None;
        }
        let (lines, input_index) = question_lines(self.question);
        let inner = Border::bordered().inner(centered_panel(area, &lines));
        let (row, col) = self.question.custom.cursor_position();
        let x =
            inner.x + u16::try_from(CUSTOM_PREFIX.width() + usize::from(col)).unwrap_or(u16::MAX);
        let y = inner.y
            + u16::try_from(input_index.expect("自定义输入行必有下标") + usize::from(row))
                .unwrap_or(u16::MAX);
        Some((x, y))
    }
}

impl Widget for QuestionOverlay<'_> {
    /// `area` 为内容区画布：先整体 [`Clear`]，再居中面板绘制。
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let (lines, _) = question_lines(self.question);
        let panel = centered_panel(area, &lines);
        let title = kind_label(self.question.prompt.kind);
        let block = Border::bordered()
            .border_type(BorderType::Plain)
            .border_style(theme::accent())
            .title(Span::styled(format!(" 提问 · {title} "), theme::accent()));
        Clear.render(panel, buf);
        let inner = block.inner(panel);
        block.render(panel, buf);
        Paragraph::new(lines).render(inner, buf);
    }
}

/// 问题类型的展示名（标题用）。
const fn kind_label(kind: QuestionKind) -> &'static str {
    match kind {
        QuestionKind::SingleChoice => "单选",
        QuestionKind::MultipleChoice => "多选",
        QuestionKind::FillIn => "填空",
    }
}

/// 面板内容行 + 自定义输入行的下标（若有）。
fn question_lines(question: &Question) -> (Vec<Line<'static>>, Option<usize>) {
    let prompt = &question.prompt;
    let mut lines = Vec::new();
    // 问题文本：按显示宽度折行（CJK 友好），不撑爆面板
    wrap(&prompt.question, MAX_LINE_WIDTH, theme::bold(), &mut lines);
    lines.push(Line::default());

    let mut input_index = None;
    match (prompt.kind, question.is_custom_input()) {
        (_, true) => {
            input_index = Some(lines.len());
            lines.push(custom_input_line(question));
        }
        (QuestionKind::FillIn, false) => {
            // 填空无选项且不在输入阶段（Esc 取消前瞬间）：占位空行
            lines.push(Line::default());
        }
        (kind, false) => {
            for (index, option) in prompt.options.iter().enumerate() {
                lines.push(option_line(question, kind, index, option));
            }
        }
    }
    lines.push(Line::default());
    lines.push(hint_line(question));
    (lines, input_index)
}

/// 自定义输入行：前缀 + 光标前文本 + `▌` + 光标后文本。
fn custom_input_line(question: &Question) -> Line<'static> {
    let text = question.custom.text();
    let cursor = usize::from(question.custom.cursor_position().1);
    let (before, after) = split_by_width(text, cursor);
    Line::from(vec![
        Span::raw(CUSTOM_PREFIX),
        Span::raw(before),
        Span::styled("▌", theme::accent()),
        Span::styled(after, theme::dim()),
    ])
}

/// 选项行：游标 `❯` + 类型标记 + 选项文本；多选已勾选行加粗，
/// 自定义选项勾选后追加已填文本。
fn option_line(
    question: &Question,
    kind: QuestionKind,
    index: usize,
    option: &str,
) -> Line<'static> {
    let highlighted = question.cursor == index;
    let marker = match kind {
        QuestionKind::SingleChoice => {
            if highlighted {
                "●"
            } else {
                "○"
            }
        }
        QuestionKind::MultipleChoice => {
            if question.selections.contains(&index) {
                "☑"
            } else {
                "☐"
            }
        }
        QuestionKind::FillIn => "",
    };
    let mut spans = vec![Span::raw(if highlighted { "❯ " } else { "  " })];
    let is_custom = option == CUSTOM_OPTION;
    let checked_custom = is_custom && question.custom_selected;
    let style = if is_custom {
        theme::accent()
    } else if highlighted {
        theme::bold()
    } else {
        theme::subtle()
    };
    spans.push(Span::styled(marker.to_string(), style));
    spans.push(Span::raw(" "));
    if is_custom {
        // 自定义选项：去除「✏️ 」图标的重复感，突出「自定义填写」语义
        let label = option.strip_prefix("✏️ ").unwrap_or(option);
        spans.push(Span::styled(label.to_string(), style));
    } else {
        spans.push(Span::styled(option.to_string(), style));
    }
    if checked_custom {
        let text = question.custom.text().trim();
        if !text.is_empty() {
            spans.push(Span::styled(format!(" → {text}"), theme::dim()));
        }
    }
    Line::from(spans)
}

/// 底部键位提示（随阶段与类型变化）。
fn hint_line(question: &Question) -> Line<'static> {
    let hint = if question.is_custom_input() {
        "Enter 提交 · Esc 取消"
    } else {
        match question.prompt.kind {
            QuestionKind::SingleChoice => "↑/↓ 选择 · Enter 提交 · Esc 取消",
            QuestionKind::MultipleChoice => "↑/↓ 选择 · 空格 勾选 · Enter 提交 · Esc 取消",
            QuestionKind::FillIn => "Enter 提交 · Esc 取消",
        }
    };
    Line::from(Span::styled(hint.to_string(), theme::dim()))
}

/// 把文本按显示宽度折行成多行（CJK 友好；超宽字符原样保留）。
fn wrap(text: &str, width: usize, style: Style, out: &mut Vec<Line<'static>>) {
    let mut current = String::new();
    let mut current_width = 0;
    for c in text.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if current_width + w > width && !current.is_empty() {
            out.push(Line::from(Span::styled(
                std::mem::take(&mut current),
                style,
            )));
            current_width = 0;
        }
        current.push(c);
        current_width += w;
    }
    if !current.is_empty() {
        out.push(Line::from(Span::styled(current, style)));
    }
}

/// 按显示宽度把文本切成（前段, 后段）：前段宽度 ≤ `width` 且尽量接近。
fn split_by_width(text: &str, width: usize) -> (String, String) {
    let mut split = text.len();
    let mut acc = 0;
    for (index, c) in text.char_indices() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if acc + w > width {
            break;
        }
        acc += w;
        split = index + c.len_utf8();
    }
    (text[..split].to_string(), text[split..].to_string())
}

#[cfg(test)]
mod tests {
    use nomic_tools::{AskUserQuestion, QuestionKind};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::tui::app::{App, Effect};
    use crate::tui::widgets::draw;

    /// 渲染一帧并提取全部非空白字符，供内容断言用。
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

    #[test]
    fn wrap_respects_display_width() {
        let mut lines = Vec::new();
        wrap("你好世界 abc", 6, theme::dim(), &mut lines);
        // 每行显示宽度 ≤ 6（CJK 每字 2 列）
        for line in &lines {
            assert!(line.width() <= 6, "{line:?}");
        }
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].width(), 6, "首行：你好世");
    }

    #[test]
    fn split_by_width_cuts_at_char_boundary() {
        let (before, after) = split_by_width("你好世界", 4);
        assert_eq!(before, "你好");
        assert_eq!(after, "世界");
        let (before, after) = split_by_width("abc", 2);
        assert_eq!(before, "ab");
        assert_eq!(after, "c");
    }

    /// 单选问题弹层：标题、问题文本、选项与游标标记（`●` 高亮）均可见；
    /// ↑/↓ 移动后标记跟随。
    #[test]
    fn renders_single_choice_question_modal() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.open_question(AskUserQuestion {
            question: "用什么语言？".to_string(),
            kind: QuestionKind::SingleChoice,
            options: vec![
                "Rust".to_string(),
                "Go".to_string(),
                CUSTOM_OPTION.to_string(),
            ],
        });

        let compact = render_compact(&mut app);
        assert!(compact.contains("提问·单选"), "{compact}");
        assert!(compact.contains("用什么语言"), "{compact}");
        assert!(compact.contains("●Rust"), "{compact}");
        assert!(compact.contains("○Go"), "{compact}");
        assert!(compact.contains("其他（自定义填写）"), "{compact}");

        // 下移后高亮标记跟随到「Go」
        app.press(crate::tui::app::Key::Down);
        let compact = render_compact(&mut app);
        assert!(compact.contains("○Rust"), "{compact}");
        assert!(compact.contains("●Go"), "{compact}");
    }

    /// 填空问题弹层：直接进入自定义输入阶段，输入文本与 `▌` 可见；
    /// Enter 提交后弹层关闭。
    #[test]
    fn renders_fill_in_question_input() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.open_question(AskUserQuestion {
            question: "邮箱？".to_string(),
            kind: QuestionKind::FillIn,
            options: Vec::new(),
        });
        app.paste_text("a@b.c");

        let compact = render_compact(&mut app);
        assert!(compact.contains("提问·填空"), "{compact}");
        assert!(compact.contains("自定义"), "{compact}");
        assert!(compact.contains("a@b.c"), "{compact}");

        // Enter 提交：弹层关闭（作答效果由事件循环接线）
        let effects = app.press(crate::tui::app::Key::Enter);
        assert!(matches!(
            effects.as_slice(),
            [Effect::SubmitQuestionAnswer(_)]
        ));
        let compact = render_compact(&mut app);
        assert!(!compact.contains("提问·填空"), "提交后弹层消失：{compact}");
    }
}
