//! 模态覆盖层 widget：键位帮助弹层（NORMAL `?`）。
//!
//! 模态覆盖层：内容区（状态栏以上）整体作为画布，先 [`Clear`]
//! 再在其中居中面板。[`HelpOverlay`] 是 [`StatefulWidget`]，渲染时把
//! 滚动偏移钳制回写（`App::help_scroll_mut` 提供的 `&mut u16`）。

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block as Border, BorderType, Clear, Paragraph, StatefulWidget, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::theme;

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
            .title(Span::styled("键位帮助 · Esc/? 关闭", theme::accent()));
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
/// 键位帮助弹层与提问弹层共用。
pub(in crate::tui) fn centered_panel(area: Rect, lines: &[Line<'static>]) -> Rect {
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
            ("Esc", "退出当前界面层（逐层退回，不中断运行）"),
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
            ("Esc", "进入 NORMAL（运行中亦然；中断在 NORMAL 按 q）"),
        ],
    ),
    (
        "NORMAL（单字母动作层）",
        &[
            ("i a Enter · A · I", "回到输入（光标原位 / 末尾 / 行首）"),
            ("j k · d u · g G", "滚动 / 半页 / 顶部 / 底部（less 式）"),
            ("Y", "复制最新一条消息"),
            ("m · r", "队列编辑 / 续跑（重发最后一条消息）"),
            ("s · b · c", "恢复会话 / 会话树（创建分支）/ 新建会话"),
            ("e · : · ?", "外部编辑器 / 命令 / 帮助"),
            ("q", "中断本轮运行（退出程序用 : 执行 quit）"),
        ],
    ),
    (
        "COMMAND（: 浮层命令栏）",
        &[
            ("Enter", "执行命令 / 展开模板（help 查看全部命令）"),
            ("Tab · ↑/↓", "补全命令 / 模板 / skill 并移动选中"),
            ("Esc", "关补全列表 / 放弃返回 NORMAL"),
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
        "PICKER",
        &[("PICKER", "输入过滤 · ↑/↓ 选择 · Home/End 首尾 · Enter/Esc")],
    ),
    (
        "提问（ask_user_question）",
        &[
            ("↑/↓ · j/k", "移动选项（循环）"),
            ("空格", "多选勾选 / 取消勾选（单选直接 Enter 提交）"),
            ("Enter", "提交（单选/多选）；自定义选项先输入文本再提交"),
            ("Esc", "取消提问（放弃自定义输入先回选项列表）"),
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
