//! 选择内核：「从列表里选一行」的纯状态（selected/offset/window + 可选过滤）。
//!
//! 本模块不碰终端，全部逻辑可单测；两个运行场景各留薄 adapter 管终端
//! 接线与键位语义（差异显式留在 adapter 层，不进内核）：
//!
//! - CLI `nomic resume` 选择器（`crate::sessions`）：↑/↓ 或 j/k 导航，
//!   无过滤，行容量随终端高度逐帧变化；
//! - TUI picker 弹层（`crate::tui::app` 的 `Picker`）：fzf 风格——可打印
//!   字符即过滤，导航全走箭头/Ctrl 键（一键一义），行可标记不可选
//!   （`/tree` 的工具调用条目），行容量固定。
//!
//! 窗口钳制（移动时维护 offset + 绘制时 [`Picker::window`] 兜底）只在
//! 本模块实现一次：单点修复，两个场景行为一致（贴边滚动）。

/// 选择器的一行：内部 id + 预生成的展示文本（渲染零计算）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub id: String,
    pub text: String,
    /// 是否可选中确认（`/tree` 的工具调用条目只展示不可选）；
    /// 其余选择器恒为 `true`
    pub selectable: bool,
}

/// 选择内核：候选行 + 选中项 + 滚动窗口起点 + 过滤串。
/// `selected`/`offset` 都是**过滤后可见行**的下标。
#[derive(Debug, Default)]
pub struct Picker {
    pub rows: Vec<PickerRow>,
    /// 当前选中项（过滤后可见行的下标）
    pub selected: usize,
    /// 滚动窗口起点（可见行下标）：移动选中时同步维护（贴边语义），
    /// 绘制时再由 [`Self::window`] 兜底钳制（终端突然变矮时 offset 可能失效）
    pub offset: usize,
    /// 过滤串（空 = 全部可见；大小写不敏感的子串匹配）
    pub filter: String,
}

impl Picker {
    /// 从头选中（对齐到首个可选行）；候选可为空（选择器对外不打开）。
    pub fn new(rows: Vec<PickerRow>) -> Self {
        let mut picker = Self {
            rows,
            ..Self::default()
        };
        picker.snap_selection();
        picker
    }

    /// 预选中 `row`（**全部行**的下标，即过滤前）；越界钳制，不可选时
    /// 对齐到最近可选行。
    pub fn with_selected(rows: Vec<PickerRow>, row: usize) -> Self {
        let mut picker = Self {
            rows,
            selected: row,
            ..Self::default()
        };
        if !picker.rows.is_empty() {
            picker.selected = picker.selected.min(picker.rows.len() - 1);
        }
        picker.snap_selection();
        picker
    }

    /// 过滤后的可见行（`rows` 下标列表，保持原顺序）。
    pub fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.rows.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        (0..self.rows.len())
            .filter(|&index| self.rows[index].text.to_lowercase().contains(&needle))
            .collect()
    }

    /// 移动选中项（在过滤后的可见行上到底/顶钳制，不循环；跳过不可选行），
    /// 并同步滚动窗口保证选中可见（`capacity` 为可见行容量）。
    pub fn select(&mut self, delta: isize, capacity: usize) {
        if delta == 0 {
            return;
        }
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
        self.ensure_visible(capacity);
    }

    /// 跳转选中到可见行的 `pos`（不可选时沿 `direction` 找可选行），
    /// 并同步滚动窗口保证选中可见。
    pub fn jump(&mut self, pos: usize, direction: isize, capacity: usize) {
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
        self.ensure_visible(capacity);
    }

    /// 实际绘制用的窗口起点：钳制在合法范围，并兜底保证选中行可见
    ///（终端突然变矮时状态里的 offset 可能失效）。
    pub fn window(&self, capacity: usize) -> usize {
        let len = self.visible().len();
        if len == 0 {
            return 0;
        }
        let capacity = capacity.min(len).max(1);
        let selected = self.selected.min(len - 1);
        self.offset
            .min(selected)
            .max(selected.saturating_sub(capacity - 1))
            .min(len - capacity)
    }

    /// 清空过滤串；返回是否确有过滤串被清空
    ///（Esc 据此决定清过滤还是关 picker）。
    pub fn clear_filter(&mut self) -> bool {
        if self.filter.is_empty() {
            return false;
        }
        self.reset_filter(String::new());
        true
    }

    /// 删除过滤串末字符（Backspace）。
    pub fn pop_filter(&mut self) {
        if self.filter.pop().is_some() {
            let filter = self.filter.clone();
            self.reset_filter(filter);
        }
    }

    /// 可打印字符即过滤（fzf 风格 adapter；无过滤场景不调用）。
    pub fn push_filter_char(&mut self, c: char) {
        self.filter.push(c);
        let filter = self.filter.clone();
        self.reset_filter(filter);
    }

    /// 当前选中行：过滤后无可见行或选中不可选行（`/tree` 的工具调用
    /// 条目）时返回 `None`（不确认、保持打开）。
    pub fn selected_row(&self) -> Option<&PickerRow> {
        let visible = self.visible();
        let &row = visible.get(self.selected)?;
        let row = &self.rows[row];
        row.selectable.then_some(row)
    }

    /// 过滤串变化：选中回顶并清零窗口，再对齐到最近可选行。
    fn reset_filter(&mut self, filter: String) {
        self.filter = filter;
        self.selected = 0;
        self.offset = 0;
        self.snap_selection();
    }

    /// 选中项对齐到最近的可选行：从当前位置向下找，找不到再向上。
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

    /// 移动后同步滚动窗口（贴边语义）：选中滚出上沿则收缩窗口，
    /// 滚出下沿则下推窗口。
    fn ensure_visible(&mut self, capacity: usize) {
        let len = self.visible().len();
        if len == 0 {
            self.offset = 0;
            return;
        }
        let capacity = capacity.max(1).min(len);
        self.offset = self.offset.min(self.selected);
        if self.selected >= self.offset + capacity {
            self.offset = self.selected + 1 - capacity;
        }
    }
}

/// 逐行步进：越过边界返回 `None`（钳制语义由调用方决定）。
/// picker 选中与队列游标共用。
pub fn step_row(index: usize, direction: isize, len: usize) -> Option<usize> {
    let next = index.checked_add_signed(direction)?;
    (next < len).then_some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(texts: &[&str]) -> Vec<PickerRow> {
        texts
            .iter()
            .map(|text| PickerRow {
                id: format!("id-{text}"),
                text: (*text).to_string(),
                selectable: true,
            })
            .collect()
    }

    // ── 选中移动与钳制 ─────────────────────────────────────────────────────

    #[test]
    fn select_up_clamps_at_top() {
        let mut picker = Picker::new(rows(&["a", "b", "c"]));
        picker.select(-1, 10);
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.offset, 0);
    }

    #[test]
    fn select_down_clamps_at_bottom() {
        let mut picker = Picker::new(rows(&["a", "b", "c"]));
        for _ in 0..10 {
            picker.select(1, 10);
        }
        assert_eq!(picker.selected, 2);
    }

    #[test]
    fn select_zero_delta_is_noop() {
        // delta 为 0 不得卡死（direction 0 的步进恒为原地）
        let mut picker = Picker::new(rows(&["a", "b"]));
        picker.select(0, 10);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn select_skips_unselectable_rows() {
        let mut list = rows(&["a", "b", "c"]);
        list[1].selectable = false;
        let mut picker = Picker::new(list);
        picker.select(1, 10);
        assert_eq!(picker.selected, 2, "下移跳过不可选行");
        picker.select(-1, 10);
        assert_eq!(picker.selected, 0, "上移同样跳过");
    }

    #[test]
    fn select_stays_when_no_selectable_ahead() {
        let mut list = rows(&["a", "b"]);
        list[1].selectable = false;
        let mut picker = Picker::new(list);
        picker.select(1, 10);
        assert_eq!(picker.selected, 0, "该方向无更多可选行则保持原位");
    }

    #[test]
    fn with_selected_preselects_and_snaps_to_selectable() {
        let picker = Picker::with_selected(rows(&["a", "b", "c"]), 2);
        assert_eq!(picker.selected, 2);

        let mut list = rows(&["a", "b", "c"]);
        list[1].selectable = false;
        let picker = Picker::with_selected(list, 1);
        assert_eq!(picker.selected, 2, "预选不可选行时向下对齐可选行");

        let picker = Picker::with_selected(rows(&["a"]), 9);
        assert_eq!(picker.selected, 0, "越界钳制");
    }

    // ── 滚动窗口 ───────────────────────────────────────────────────────────

    #[test]
    fn select_down_scrolls_window_to_keep_selection_visible() {
        let mut picker = Picker::new(rows(&["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]));
        for _ in 0..5 {
            picker.select(1, 3);
        }
        assert_eq!(picker.selected, 5);
        assert_eq!(picker.offset, 3, "选中行应贴在窗口下沿");
        assert_eq!(picker.window(3), 3);
    }

    #[test]
    fn select_up_scrolls_window_back() {
        let mut picker = Picker::new(rows(&["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]));
        picker.jump(5, 1, 3);
        assert_eq!(picker.offset, 3);
        for _ in 0..4 {
            picker.select(-1, 3);
        }
        assert_eq!(picker.selected, 1);
        assert_eq!(picker.offset, 1, "选中行应贴在窗口上沿");
    }

    #[test]
    fn window_keeps_selection_visible_after_shrink() {
        // 终端变矮导致容量缩小：旧 offset 让选中行跑出窗口时兜底修正
        let mut picker = Picker::new(rows(&["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]));
        picker.jump(9, 1, 6);
        assert_eq!(picker.offset, 4);
        assert_eq!(picker.window(3), 7);
    }

    #[test]
    fn window_handles_zero_capacity_and_empty_list() {
        let mut picker = Picker::default();
        assert_eq!(picker.window(0), 0);
        picker.select(1, 0);
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.offset, 0);

        let mut picker = Picker::new(rows(&["a", "b", "c"]));
        picker.select(1, 0);
        assert_eq!(picker.selected, 1);
        assert_eq!(picker.window(0), 1, "容量 0 按至少一行保证选中行可见");
    }

    #[test]
    fn single_row_stays_on_first_row() {
        let mut picker = Picker::new(rows(&["a"]));
        picker.select(1, 1);
        picker.select(-1, 1);
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.window(1), 0);
    }

    // ── 过滤 ───────────────────────────────────────────────────────────────

    #[test]
    fn filter_narrows_visible_rows_case_insensitively() {
        let mut picker = Picker::new(rows(&["alpha session", "beta session", "beta branch"]));
        for c in "BETA".chars() {
            picker.push_filter_char(c);
        }
        assert_eq!(picker.visible(), vec![1, 2]);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn filter_change_resets_selection_and_window() {
        let mut picker = Picker::new(rows(&["0", "1", "2", "3", "4", "5"]));
        picker.jump(5, 1, 3);
        assert_eq!(picker.offset, 3);
        picker.push_filter_char('5');
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.offset, 0, "过滤变化后窗口回顶");
    }

    #[test]
    fn pop_and_clear_filter_restore_full_list() {
        let mut picker = Picker::new(rows(&["alpha", "beta"]));
        picker.push_filter_char('x');
        assert!(picker.visible().is_empty());
        picker.pop_filter();
        assert_eq!(picker.visible().len(), 2);

        picker.push_filter_char('a');
        assert!(picker.clear_filter());
        assert_eq!(picker.visible().len(), 2);
        assert!(!picker.clear_filter(), "空过滤串再次清空返回 false");
    }

    #[test]
    fn selected_row_none_when_no_match_or_unselectable() {
        let mut picker = Picker::new(rows(&["alpha", "beta"]));
        assert_eq!(
            picker.selected_row().map(|row| row.id.as_str()),
            Some("id-alpha")
        );

        for c in "zzz".chars() {
            picker.push_filter_char(c);
        }
        assert_eq!(picker.selected_row(), None, "无匹配行不确认");

        let mut list = rows(&["a"]);
        list[0].selectable = false;
        let picker = Picker::new(list);
        assert_eq!(picker.selected_row(), None, "不可选行不确认");
    }

    // ── step_row ───────────────────────────────────────────────────────────

    #[test]
    fn step_row_stops_at_boundaries() {
        assert_eq!(step_row(0, 1, 3), Some(1));
        assert_eq!(step_row(2, 1, 3), None);
        assert_eq!(step_row(0, -1, 3), None);
    }
}
