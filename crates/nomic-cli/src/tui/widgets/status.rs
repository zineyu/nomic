//! 状态栏 widget：模式徽标 + 模型徽标 + 上下文用量 + 告警；右侧滚动位置与键位提示。
//!
//! [`StatusBar`] 是 [`Widget`]：从 [`App`] 只读构建单行状态栏。
//! [`format_tokens`] 同时被模型切换的效果层（`effects::model`）复用。

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::tui::app::{App, Mode};
use crate::tui::theme;

/// 状态栏 widget：从 [`App`] 只读构建单行画面。
pub(in crate::tui) struct StatusBar<'a> {
    app: &'a App,
}

impl<'a> StatusBar<'a> {
    pub(in crate::tui) const fn new(app: &'a App) -> Self {
        Self { app }
    }
}

impl Widget for StatusBar<'_> {
    /// 渲染状态栏：左侧模式徽标 + 模型徽标 + 上下文用量 + 告警；
    /// 右侧滚动位置 + 随模式切换的键位提示。
    fn render(self, area: Rect, buf: &mut Buffer) {
        let app = self.app;
        // 模式徽标（ADR-0011）：NORMAL 反色绿块强提示；INSERT/PICKER 低调
        // accent 文本，避免与相邻的模型徽标（反色块）糊成一片
        let mode_badge = match app.mode() {
            Mode::Normal => Span::styled(" NORMAL ", theme::normal_badge()),
            Mode::Command => Span::styled(" COMMAND ", theme::warn()),
            Mode::Queue => Span::styled(" QUEUE ", theme::queue_badge()),
            Mode::Insert => Span::styled(" INSERT ", theme::accent()),
            Mode::Picker => Span::styled(" PICKER ", theme::accent()),
            Mode::Help => Span::styled(" HELP ", theme::accent()),
            Mode::Question => Span::styled(" 提问 ", theme::accent()),
        };
        let mut left = vec![
            mode_badge,
            Span::styled(format!(" {} ", app.model_name()), theme::selected()),
            context_usage_span(app),
        ];
        // goal 模式开启时给出常驻徽标：自动追问进行中用户能看到原因
        if app.goal_mode() {
            left.push(Span::styled(" goal ", theme::warn()));
        }
        if let Some(notice) = app.notice() {
            left.push(Span::styled(format!("⚠ {notice} "), theme::warn()));
        }
        let mut right = Vec::new();
        if app.chat().scroll() > 0 {
            right.push(Span::styled(
                format!("↑ {}/{} ", app.chat().scroll(), app.chat().scroll_max()),
                theme::warn(),
            ));
        }
        // 键位提示保持精简：完整键位见欢迎页与 /help，此处只留模式核心键
        let hint = match app.mode() {
            Mode::Normal => "i 输入 · : 命令 · q 中断 · ? 帮助 ",
            Mode::Command => "Tab 补全 · Enter 执行 · Esc 返回 ",
            Mode::Picker => "输入过滤 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ",
            Mode::Help => "j/k 滚动 · g/G 顶/底 · Esc 关闭 ",
            Mode::Insert => "Enter 发送 · ^G 编辑器 · Esc 浏览 ",
            Mode::Question => "↑/↓ 选择 · Enter 提交 · 空格 勾选 · Esc 取消 ",
            Mode::Queue => {
                if app.queue().is_editing() {
                    "Enter/Esc 保存 · Shift+Enter 换行 "
                } else {
                    "j/k 移动 · i 编辑 · dd 删除 · J/K 换位 · o 新增 "
                }
            }
        };
        right.push(Span::styled(hint, theme::dim()));
        let left_line = Line::from(left);
        let right_line = Line::from(right);
        // 宽度不足时省略右侧提示，避免与左侧信息交叠
        if left_line.width() + right_line.width() <= usize::from(area.width) {
            Paragraph::new(right_line)
                .alignment(Alignment::Right)
                .render(area, buf);
        }
        Paragraph::new(left_line).render(area, buf);
    }
}

/// 状态栏上下文用量：常态紧凑显示占比（`ctx 6%`）；窗口未知（0）时
/// 只显示 token 估算。用量逼近窗口（≥80%）时展开完整数值并以警告色
/// 提示（`ctx 168k/200k·84%`）。
fn context_usage_span(app: &App) -> Span<'static> {
    let tokens = app.context_tokens();
    let window = app.context_window();
    if window == 0 {
        return Span::styled(format!(" ctx {} ", format_tokens(tokens)), theme::dim());
    }
    let percent = tokens.saturating_mul(100) / window;
    if percent >= 80 {
        return Span::styled(
            format!(
                " ctx {}/{}·{}% ",
                format_tokens(tokens),
                format_tokens(window),
                percent
            ),
            theme::warn(),
        );
    }
    Span::styled(format!(" ctx {percent}% "), theme::dim())
}

/// token 数紧凑格式：<1k 原样，<10k 一位小数（`8.4k`），其余取整（`200k`）。
pub(in crate::tui) fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 10_000 {
        let deci_k = tokens / 100;
        format!("{}.{}k", deci_k / 10, deci_k % 10)
    } else {
        format!("{}k", tokens / 1_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// token 数紧凑格式：<1k 原样，<10k 一位小数，其余取整。
    #[test]
    fn formats_tokens_compactly() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(843), "843");
        assert_eq!(format_tokens(1_000), "1.0k");
        assert_eq!(format_tokens(8_432), "8.4k");
        assert_eq!(format_tokens(12_300), "12k");
        assert_eq!(format_tokens(200_000), "200k");
    }
}
