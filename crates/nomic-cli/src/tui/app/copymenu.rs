//! 复制菜单（NORMAL `y` 打开的 overlay，ADR-0021）：可复制条目列表与选中。
//!
//! [`CopyMenu`] 打开时从聊天条目快照构建行（消息与代码块，新条目在前）；
//! 确认产出 [`super::Effect::CopyText`]，提示语由模式路由层落到状态栏。

use super::chat::{ChatItem, item_text};

/// 复制菜单的一行：展示标签 + 复制文本（打开时快照，渲染零计算）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct CopyRow {
    /// 展示标签（角色与摘要，如「助手 · 看这里：…」「代码块 1/2」）
    pub(in crate::tui) label: String,
    /// 确认时复制的完整文本
    text: String,
}

/// 复制菜单状态：快照行 + 当前选中（新条目在前，选中预定位到消息游标
/// 所在条目的行，近似原 `yy` 的「复制当前条目」）。
#[derive(Debug)]
pub(in crate::tui) struct CopyMenu {
    rows: Vec<CopyRow>,
    selected: usize,
}

impl CopyMenu {
    /// `cursor` 是消息游标（items 下标），选中预定位到该条目的首行。
    /// 没有任何可复制内容时返回 `None`。
    pub(super) fn build(items: &[ChatItem], cursor: Option<usize>) -> Option<Self> {
        let mut rows = Vec::new();
        // 消息游标所在条目在 rows 中的首行下标（预选中用）
        let mut cursor_row = None;
        for (index, item) in items.iter().enumerate().rev() {
            let Some(text) = item_text(item) else {
                continue;
            };
            let label = format!("{} · {}", role_label(item), first_line(&text));
            if cursor == Some(index) {
                cursor_row = Some(rows.len());
            }
            rows.push(CopyRow { label, text });
        }
        if rows.is_empty() {
            return None;
        }
        Some(Self {
            rows,
            selected: cursor_row.unwrap_or(0),
        })
    }

    /// 菜单行（渲染用）。
    pub(in crate::tui) fn rows(&self) -> &[CopyRow] {
        &self.rows
    }

    /// 当前选中行下标（渲染用）。
    pub(in crate::tui) const fn selected(&self) -> usize {
        self.selected
    }

    /// 移动选中（钳制不循环）。
    pub(super) fn select(&mut self, delta: isize) {
        let len = self.rows.len();
        if delta < 0 {
            self.selected = self.selected.saturating_sub(delta.unsigned_abs());
        } else {
            self.selected = self
                .selected
                .saturating_add(delta.unsigned_abs())
                .min(len.saturating_sub(1));
        }
    }

    /// 跳到首行（`g`）。
    pub(super) const fn jump_first(&mut self) {
        self.selected = 0;
    }

    /// 跳到末行（`G`）。
    pub(super) const fn jump_last(&mut self) {
        self.selected = self.rows.len() - 1;
    }

    /// 数字键直达（`1`-`9`，按下标）：越界返回 `None`。
    #[allow(clippy::missing_const_for_fn)]
    pub(super) fn select_index(&mut self, index: usize) -> Option<String> {
        let row = self.rows.get(index)?;
        self.selected = index;
        Some(row.text.clone())
    }

    /// 当前选中行的复制文本（Enter 确认）。
    #[allow(clippy::missing_const_for_fn)]
    pub(super) fn selected_text(&self) -> String {
        self.rows[self.selected].text.clone()
    }
}

/// 条目的角色标签（复制菜单行前缀用）。
const fn role_label(item: &ChatItem) -> &'static str {
    match item {
        ChatItem::User(_) => "用户",
        ChatItem::Assistant(_) => "助手",
        ChatItem::Tool(_) => "工具",
        ChatItem::System(_) => "系统",
    }
}

/// 文本首行（截断到 40 列内，菜单摘要用）。
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("");
    let mut summary: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        summary.push('…');
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatItem {
        ChatItem::User(text.to_string())
    }

    fn assistant(text: &str) -> ChatItem {
        ChatItem::Assistant(super::super::chat::AssistantItem {
            blocks: vec![super::super::chat::Block::Text(text.to_string())],
            done: true,
            error: None,
            collapsed: false,
        })
    }

    /// 菜单构建：新条目在前、代码块单独成行、游标行预选。
    #[test]
    fn build_orders_newest_first_and_presets_cursor_row() {
        let items = vec![
            user("第一个问题"),
            assistant("回答一"),
            user("第二个问题"),
            assistant("看这里：\n```rust\nfn main() {}\n```\n还有：\n```\n第二块\n```"),
        ];
        // 游标在最早的 user（下标 0）：预选中其所在行
        let menu = CopyMenu::build(&items, Some(0)).expect("有内容");
        let labels: Vec<&str> = menu.rows().iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "助手 · 看这里：",
                "用户 · 第二个问题",
                "助手 · 回答一",
                "用户 · 第一个问题",
            ],
            "{labels:?}"
        );
        assert_eq!(menu.selected(), 3);
        // 数字键直达与 Enter 确认
        let mut menu = CopyMenu::build(&items, None).expect("有内容");
        assert_eq!(
            menu.selected_text(),
            "看这里：\n```rust\nfn main() {}\n```\n还有：\n```\n第二块\n```"
        );
        assert_eq!(menu.select_index(1).as_deref(), Some("第二个问题"));
        assert_eq!(menu.selected(), 1);
        assert!(menu.select_index(9).is_none(), "越界返回 None");

        // 空聊天：无菜单
        assert!(CopyMenu::build(&[], None).is_none());
    }

    /// 选中导航：钳制不循环，g/G 跳首尾。
    #[test]
    fn select_clamps_and_jumps() {
        let items = vec![user("甲"), user("乙"), user("丙")];
        let mut menu = CopyMenu::build(&items, None).expect("有内容");
        menu.select(-1);
        assert_eq!(menu.selected(), 0, "顶部钳制");
        menu.select(1);
        menu.select(1);
        menu.select(1);
        assert_eq!(menu.selected(), 2, "底部钳制");
        menu.jump_first();
        assert_eq!(menu.selected(), 0);
        menu.jump_last();
        assert_eq!(menu.selected(), 2);
    }
}
