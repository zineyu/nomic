//! 选择器状态（`/resume`、`/models`、`/tree` 共用）：候选行、过滤与选中。
//!
//! [`Picker`] 自持候选与选中导航；确认产出的 [`super::Effect`] 由
//! 模式路由层（[`super::App`]）按 [`PickerKind`] 分发。

use super::step_row;

/// picker Ctrl+D/Ctrl+U 的半页翻步长（可见行下标计）。
pub(super) const PICKER_PAGE_SCROLL: isize = 10;

/// 选择器的一行：内部 id + 预生成的展示文本（渲染零计算）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct PickerRow {
    pub(in crate::tui) id: String,
    pub(in crate::tui) text: String,
    /// 是否可选中确认（`/tree` 的工具调用条目只展示不可选）；
    /// 其余选择器恒为 `true`
    pub(in crate::tui) selectable: bool,
}

/// 选择器种类：决定确认动作（[`super::Effect`]）与渲染标题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum PickerKind {
    /// `/resume`：恢复历史 session
    Resume,
    /// `/tree`：选择分支起点
    Tree,
    /// `/models`：切换模型
    Models,
    /// 模型切换流程第二步：设置思考级别
    Reasoning,
    /// NORMAL `s`：会话菜单（恢复 / 新建 / 分支树合一入口）
    Session,
}

/// 选择器状态：候选行 + 当前选中项 + 过滤串（fzf 风格：可打印字符即过滤，
/// ↑/↓ 导航）。`selected` 是**过滤后可见行**的下标。
#[derive(Debug)]
pub(in crate::tui) struct Picker {
    pub(in crate::tui) kind: PickerKind,
    pub(in crate::tui) rows: Vec<PickerRow>,
    pub(in crate::tui) selected: usize,
    /// 过滤串（空 = 全部可见；大小写不敏感的子串匹配）
    pub(in crate::tui) filter: String,
}

impl Picker {
    /// 打开 `/resume` 选择器（从头选中）；调用方保证候选非空。
    pub(super) fn resume(rows: Vec<PickerRow>) -> Self {
        debug_assert!(!rows.is_empty());
        Self {
            kind: PickerKind::Resume,
            rows,
            selected: 0,
            filter: String::new(),
        }
    }

    /// 打开 `/models` 选择器，预选中当前模型；调用方保证候选非空。
    pub(super) fn models(rows: Vec<PickerRow>, selected: usize) -> Self {
        debug_assert!(!rows.is_empty());
        debug_assert!(selected < rows.len());
        Self {
            kind: PickerKind::Models,
            rows,
            selected,
            filter: String::new(),
        }
    }

    /// 打开思考级别选择器（模型切换流程第二步，预选中当前级别）；
    /// 调用方保证候选非空。
    pub(super) fn reasoning(rows: Vec<PickerRow>, selected: usize) -> Self {
        debug_assert!(!rows.is_empty());
        debug_assert!(selected < rows.len());
        Self {
            kind: PickerKind::Reasoning,
            rows,
            selected,
            filter: String::new(),
        }
    }

    /// 打开 `/tree` 选择器（预选中 `selected`，通常是当前分支末端）；
    /// 调用方保证候选非空且 `selected` 落在可选行上。
    pub(super) fn tree(rows: Vec<PickerRow>, selected: usize) -> Self {
        debug_assert!(!rows.is_empty());
        debug_assert!(rows[selected].selectable);
        Self {
            kind: PickerKind::Tree,
            rows,
            selected,
            filter: String::new(),
        }
    }

    /// 打开会话菜单（NORMAL `s`）：固定三项，从头选中。
    pub(super) fn session() -> Self {
        let rows = [
            ("resume", "恢复历史 session"),
            ("new", "新建对话（清空上下文）"),
            ("tree", "会话树（创建分支）"),
        ]
        .map(|(id, text)| PickerRow {
            id: id.to_string(),
            text: text.to_string(),
            selectable: true,
        })
        .to_vec();
        Self {
            kind: PickerKind::Session,
            rows,
            selected: 0,
            filter: String::new(),
        }
    }

    /// 过滤后的可见行（`rows` 下标列表，保持原顺序）。
    pub(in crate::tui) fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.rows.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        (0..self.rows.len())
            .filter(|&index| self.rows[index].text.to_lowercase().contains(&needle))
            .collect()
    }

    /// 移动选中项（在过滤后的可见行上到底/顶钳制，不循环；跳过不可选行）。
    pub(super) fn select(&mut self, delta: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let direction: isize = delta.signum();
        let mut pos = self.selected.min(visible.len() - 1);
        for _ in 0..delta.unsigned_abs() {
            let Some(next) = step_row(pos, direction, visible.len()) else {
                break;
            };
            pos = next;
        }
        // 落点不可选时沿移动方向继续；该方向上没有更多可选行则保持原位
        while !self.rows[visible[pos]].selectable {
            let Some(next) = step_row(pos, direction, visible.len()) else {
                return;
            };
            pos = next;
        }
        self.selected = pos;
    }

    /// 跳转选中到可见行的 `pos`，不可选时沿 `direction` 找可选行。
    pub(super) fn jump(&mut self, pos: usize, direction: isize) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let mut pos = pos.min(visible.len() - 1);
        while !self.rows[visible[pos]].selectable {
            let Some(next) = step_row(pos, direction, visible.len()) else {
                return;
            };
            pos = next;
        }
        self.selected = pos;
    }

    /// 选中项对齐到最近的可选行（过滤变化后调用）：从当前位置向下找，
    /// 找不到再向上。
    fn snap_selection(&mut self) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let pos = self.selected.min(visible.len() - 1);
        let snapped = (pos..visible.len())
            .chain((0..pos).rev())
            .find(|&p| self.rows[visible[p]].selectable);
        self.selected = snapped.unwrap_or(pos);
    }

    /// 清空过滤串；返回是否确有过滤串被清空
    ///（Esc 据此决定清过滤还是关 picker）。
    pub(super) fn clear_filter(&mut self) -> bool {
        if self.filter.is_empty() {
            return false;
        }
        self.filter.clear();
        self.selected = 0;
        self.snap_selection();
        true
    }

    /// 删除过滤串末字符（Backspace）。
    pub(super) fn pop_filter(&mut self) {
        if self.filter.pop().is_some() {
            self.selected = 0;
            self.snap_selection();
        }
    }

    /// 可打印字符即过滤（含 j/k/q——导航全部走箭头/Ctrl 键，一键一义）。
    pub(super) fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        self.selected = 0;
        self.snap_selection();
    }

    /// 当前选中行的（种类, id）：过滤后无可见行或选中不可选行
    ///（`/tree` 的工具调用条目）时返回 `None`（不确认、保持打开）。
    pub(super) fn selected_entry(&self) -> Option<(PickerKind, String)> {
        let visible = self.visible();
        let &row = visible.get(self.selected)?;
        if !self.rows[row].selectable {
            return None;
        }
        Some((self.kind, self.rows[row].id.clone()))
    }
}
