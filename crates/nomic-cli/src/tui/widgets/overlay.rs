//! 模态覆盖层 widget：复制菜单（NORMAL `y`）与键位帮助弹层（NORMAL `?`）。
//!
//! 两者都是模态覆盖层：内容区（状态栏以上）整体作为画布，先 [`Clear`]
//! 再在其中居中面板。[`CopyMenuOverlay`] 是 [`Widget`]（只读快照）；
//! [`HelpOverlay`] 是 [`StatefulWidget`]，渲染时把滚动偏移钳制回写
//! （`App::help_scroll_mut` 提供的 `&mut u16`）。

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph, StatefulWidget, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::CopyMenu;
use crate::tui::theme;

/// 复制菜单（NORMAL `y`）：模态覆盖层，居中面板列出可复制条目；
/// 选中行高亮，Enter/数字键复制、Esc/q 关闭。
pub(in crate::tui) struct CopyMenuOverlay<'a> {
    menu: &'a CopyMenu,
}

impl<'a> CopyMenuOverlay<'a> {
    pub(in crate::tui) const fn new(menu: &'a CopyMenu) -> Self {
        Self { menu }
    }
}

impl Widget for CopyMenuOverlay<'_> {
    /// `area` 为内容区画布：先整体 [`Clear`]，再居中面板。
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        let menu = self.menu;
        let rows = menu.rows();
        let selected = menu.selected();
        let lines: Vec<Line<'static>> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let prefix = if index == selected { "▸ " } else { "  " };
                let style = if index == selected {
                    theme::selected()
                } else {
                    theme::dim()
                };
                Line::from(Span::styled(format!("{prefix}{}", row.label), style))
            })
            .collect();
        let panel = centered_panel(area, &lines);
        let block = Border::bordered()
            .border_type(BorderType::Plain)
            .border_style(theme::accent())
            .title(Span::styled(
                "复制 · j/k 选择 · 1-9 直达 · Enter 确认 · Esc 关闭",
                theme::accent(),
            ));
        Clear.render(panel, buf);
        let inner = block.inner(panel);
        block.render(panel, buf);
        Paragraph::new(lines).render(inner, buf);
    }
}

/// 键位帮助弹层（NORMAL `?`）：模态覆盖层，内容超长时 j/k 等滚动。
/// [`StatefulWidget`]：渲染时把滚动偏移钳制到内容上限并回写。
pub(in crate::tui) struct HelpOverlay;

impl StatefulWidget for HelpOverlay {
    /// 帮助弹层滚动偏移（`App::help_scroll_mut` 提供）。
    type State = u16;

    /// `area` 为内容区画布：先整体 [`Clear`]，再居中面板；
    /// 滚动偏移钳制到内容上限后回写 `state`。
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        Clear.render(area, buf);
        let lines = help_lines();
        let panel = centered_panel(area, &lines);
        let block = Border::bordered()
            .border_type(BorderType::Plain)
            .border_style(theme::accent())
            .title(Span::styled("键位帮助 · Esc/q/? 关闭", theme::accent()));
        Clear.render(panel, buf);
        let inner = block.inner(panel);
        block.render(panel, buf);
        let max_scroll = u16::try_from(lines.len().saturating_sub(usize::from(inner.height)))
            .unwrap_or(u16::MAX);
        *state = (*state).min(max_scroll);
        Paragraph::new(lines).scroll((*state, 0)).render(inner, buf);
    }
}

/// 居中面板：宽高取内容与可用区域的较小值（边框 + 左右留白各一列），居中。
fn centered_panel(area: Rect, lines: &[Line<'static>]) -> Rect {
    let max_line_width = lines
        .iter()
        .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(0);
    let width = max_line_width
        .saturating_add(3)
        .min(area.width.saturating_sub(2));
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// 帮助弹层内容（NORMAL `?`）：分组键位表，与 README「TUI 键位」一致。
const HELP_GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "通用",
        &[
            ("Esc", "INSERT→NORMAL；NORMAL 运行中中断 / 空闲回 INSERT"),
            ("Ctrl+C", "清草稿 → 再按退出"),
            ("Ctrl+D", "草稿为空时退出"),
            ("PgUp/PgDn · 滚轮", "滚动聊天区"),
            ("Shift+拖选", "复制文本（TUI 捕获鼠标）"),
        ],
    ),
    (
        "INSERT（输入）",
        &[
            ("Enter", "发送（运行中排入队列）"),
            ("Shift+Enter · Ctrl+J", "手动换行"),
            ("↑/↓", "输入历史召回"),
            ("Ctrl+W · Ctrl+U", "删词 / 清行"),
            ("Ctrl+A/E · Alt+B/F", "行首行尾 / 词级移动"),
            ("Ctrl+G", "外部编辑器（$VISUAL/$EDITOR）编辑草稿"),
            ("Ctrl+V", "粘贴剪贴板图片"),
            (
                "Esc",
                "进入 NORMAL（运行中亦然；中断在 NORMAL 按 q 或再按 Esc）",
            ),
        ],
    ),
    (
        "NORMAL（单字母动作层）",
        &[
            ("i a Enter · A · I", "回到输入（光标原位 / 末尾 / 行首）"),
            ("j k · d u · g G", "滚动 / 半页 / 顶部 / 底部（less 式）"),
            ("[ ] · { }", "上/下一条消息 · 上/下一个工具调用"),
            ("/ · n · N", "聊天搜索与跳转"),
            ("y · Y", "复制菜单 / 直接复制最新消息"),
            ("Space", "折叠/展开当前条目"),
            ("m · r", "队列编辑 / 重试最近一轮"),
            ("s · b · c", "恢复会话 / 会话树（创建分支）/ 新建会话"),
            ("e · : · ?", "外部编辑器 / 命令 / 帮助"),
            ("q", "运行中中断本轮（留在 NORMAL）/ 空闲退出"),
        ],
    ),
    (
        "复制菜单（y）",
        &[
            ("j k · g G", "选择 / 首 / 尾"),
            ("1-9", "数字键直达复制对应行"),
            ("Enter", "复制选中行并关闭"),
            ("Esc · q", "关闭"),
        ],
    ),
    (
        "COMMAND（:）",
        &[
            ("Enter", "执行命令 / 展开模板（/help 查看全部命令）"),
            ("Tab · ↑/↓", "补全命令 / 模板 / skill 并移动选中"),
            ("Esc", "关补全弹层 / 放弃返回 NORMAL"),
            ("编辑键", "与 INSERT 相同（词级移动、删词等）"),
        ],
    ),
    (
        "QUEUE（m · 队列编辑）",
        &[
            ("j/k · g · G", "移动条目游标 / 队首 / 队尾"),
            (
                "i · o · O · Enter",
                "就地编辑 / 下方 / 上方新增（Enter/Esc 保存）",
            ),
            ("dd · x", "删除条目"),
            ("J · K", "下移 / 上移（换位）"),
            ("Esc", "返回（恢复发送）"),
        ],
    ),
    (
        "SEARCH · PICKER",
        &[
            ("SEARCH", "输入即搜 · Enter 完成 · Esc 取消"),
            ("PICKER", "输入过滤 · ↑/↓ 选择 · Home/End 首尾 · Enter/Esc"),
        ],
    ),
];

/// 键位列的目标显示宽度（键名左对齐，描述另起一栏）。
const HELP_KEY_COL: usize = 26;

/// 帮助弹层的全部内容行（键名列按显示宽度对齐，CJK 友好）。
fn help_lines() -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, (title, rows)) in HELP_GROUPS.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(format!(" {title}"), theme::bold())));
        for (keys, desc) in *rows {
            let pad = HELP_KEY_COL.saturating_sub(UnicodeWidthStr::width(*keys));
            lines.push(Line::from(vec![
                Span::styled(format!("  {keys}{:pad$}", ""), theme::accent()),
                Span::styled((*desc).to_string(), theme::dim()),
            ]));
        }
    }
    lines
}
