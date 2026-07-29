//! TUI 主题：语义调色板，渲染层唯一取色入口。
//!
//! 样式一律按语义命名（而非颜色名）；后续换肤或 `NO_COLOR` 适配只改本文件。

use ratatui::style::{Color, Modifier, Style};

/// 品牌强调色：用户消息标记、选中态、焦点边框。
pub(super) const ACCENT: Color = Color::Cyan;
/// 辅助信息：thinking、摘要、系统提示、非焦点边框。
pub(super) const DIM: Color = Color::DarkGray;
/// 次要正文：补全弹层未选中项。
pub(super) const SUBTLE: Color = Color::Gray;
/// 成功。
pub(super) const OK: Color = Color::Green;
/// 失败 / 错误。
pub(super) const ERR: Color = Color::Red;
/// 进行中 / 警告。
pub(super) const BUSY: Color = Color::Yellow;

/// 加粗正文（工具名等）。
pub(super) const fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// 辅助文本（工具参数与摘要、系统提示、空状态引导）。
pub(super) const fn dim() -> Style {
    Style::new().fg(DIM)
}

/// 次要文本（补全弹层未选中项）。
pub(super) const fn subtle() -> Style {
    Style::new().fg(SUBTLE)
}

/// thinking 块：暗色斜体。
pub(super) const fn thinking() -> Style {
    dim().add_modifier(Modifier::ITALIC)
}

/// 用户消息左侧竖条标记。
pub(super) const fn user_marker() -> Style {
    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
}

/// 用户消息正文。
pub(super) const fn user_text() -> Style {
    Style::new().fg(ACCENT)
}

/// 工具进行中标记。
pub(super) const fn busy() -> Style {
    Style::new().fg(BUSY)
}

/// 工具成功标记。
pub(super) const fn ok() -> Style {
    Style::new().fg(OK)
}

/// 错误文本（失败详情、assistant 错误行）。
pub(super) const fn err() -> Style {
    Style::new().fg(ERR)
}

/// 加粗错误（失败的工具名）。
pub(super) const fn err_bold() -> Style {
    err().add_modifier(Modifier::BOLD)
}

/// 警告文本（状态栏提示）。
pub(super) const fn warn() -> Style {
    Style::new().fg(BUSY)
}

/// 反色 accent 块（选中项、状态栏徽标）。
pub(super) const fn selected() -> Style {
    Style::new()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}
