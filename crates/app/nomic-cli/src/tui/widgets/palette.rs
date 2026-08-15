//! 浮层命令栏 widget（COMMAND 模式，ADR-0020 修订）。
//!
//! [`CommandPalette`] 是覆盖层 [`Widget`]：屏幕中上方水平居中的单行
//! 输入框（`:` 提示符 + 命令文本），补全候选列在输入行下方（同一边框
//! 内，进入即列出全部命令）。命令栏不复用聊天输入区——草稿在命令栏
//! 打开期间保持可见。光标位置在渲染后由组合根经
//! [`CommandPalette::cursor_position`] 计算并设置（与输入框同一分工）。

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph, Widget},
};

use crate::tui::app::{CompletionCandidate, Input};
use crate::tui::theme;

use super::popup::visible_window;

/// 命令栏候选可见行数上限，超出时内部滚动窗口（与补全弹层同一口径）。
const PALETTE_MAX_VISIBLE: usize = 10;

/// 命令栏宽度下限（宽屏取屏宽 3/5，窄屏按屏宽收缩）。
const PALETTE_MIN_WIDTH: u16 = 24;

/// 距屏幕顶部的行数（中上方浮层）。
const PALETTE_TOP_OFFSET: u16 = 2;

/// 浮层命令栏 widget：从命令缓冲只读构建画面。
pub(in crate::tui) struct CommandPalette<'a> {
    command: &'a Input,
}

impl<'a> CommandPalette<'a> {
    pub(in crate::tui) const fn new(command: &'a Input) -> Self {
        Self { command }
    }

    /// 浮层几何（渲染与光标定位共用）：宽度为屏宽的 3/5（钳制
    /// [24, 屏宽-4]），水平居中，贴近顶部；高度 = 输入行 + 可见候选行
    /// + 上下边框。
    fn geometry(&self, screen: Rect) -> Rect {
        let max_width = screen.width.saturating_sub(4).max(1);
        let width = (screen.width * 3 / 5).max(PALETTE_MIN_WIDTH).min(max_width);
        let candidates = self
            .command
            .completion()
            .map_or(0, |completion| completion.candidates.len());
        let visible = candidates.min(PALETTE_MAX_VISIBLE);
        let height = u16::try_from(1 + visible).unwrap_or(u16::MAX) + 2;
        let height = height.min(screen.height).max(1);
        let x = screen.x + screen.width.saturating_sub(width) / 2;
        let y = screen.y + PALETTE_TOP_OFFSET.min(screen.height.saturating_sub(height));
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// 光标定位（渲染后由组合根设置）：定位在 `:` 提示符后的文本处；
    /// 长行贴右边界截断（不横向滚动），与输入框同一口径。
    pub(in crate::tui) fn cursor_position(&self, screen: Rect) -> Position {
        let area = self.geometry(screen);
        let (_, col) = self.command.cursor_position();
        // `:` 提示符占 1 列；文本可用宽度再减 1
        let x = area.x + 1 + (col + 1).min(area.width.saturating_sub(2));
        Position::new(x, area.y + 1)
    }
}

impl Widget for CommandPalette<'_> {
    /// 渲染浮层命令栏：首行 `:` 提示符 + 命令文本，下方为补全候选
    ///（选中项以 `❯` 前缀标出；无候选时仅输入行）。
    fn render(self, screen: Rect, buf: &mut Buffer) {
        let area = self.geometry(screen);
        let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
            Span::styled(":", theme::accent()),
            Span::raw(self.command.text().to_string()),
        ])];
        let mut title = "命令".to_string();
        if let Some(completion) = self.command.completion() {
            let total = completion.candidates.len();
            let (start, end) = visible_window(total, completion.selected, PALETTE_MAX_VISIBLE);
            if total > PALETTE_MAX_VISIBLE {
                title = format!("命令 {}/{total}", completion.selected + 1);
            }
            lines.extend(completion.candidates[start..end].iter().enumerate().map(
                |(offset, candidate)| {
                    let text = match candidate {
                        CompletionCandidate::Command(command) => {
                            format!("{:<8} {}", command.name, command.summary)
                        }
                        CompletionCandidate::Template(template) => match &template.argument_hint {
                            Some(hint) => format!(
                                "{:<10} {:<14} {}",
                                template.name, hint, template.description
                            ),
                            None => format!("{:<10} {}", template.name, template.description),
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
                },
            ));
        }
        let block = Border::bordered()
            .border_type(BorderType::Plain)
            .border_style(theme::accent())
            .title(Span::styled(title, theme::accent()));
        Clear.render(area, buf);
        Paragraph::new(lines).block(block).render(area, buf);
    }
}
