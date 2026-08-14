//! 选择器 adapter（`resume`、`models`、`tree` 命令共用）：选择内核
//! （[`crate::picker::Picker`]）+ 种类（决定确认动作与渲染标题）。
//!
//! 键位语义是本 adapter 的显式选择（与 CLI `nomic resume` 选择器不同：
//! 那边 ↑/↓ 或 j/k 导航、无过滤）：fzf 风格——可打印字符即过滤，导航
//! 全走箭头/Ctrl 键（一键一义），行可标记不可选（`tree` 的工具调用
//! 条目）。确认产出的 [`super::Effect`] 由模式路由层（[`super::App`]）
//! 按 [`PickerKind`] 分发。

use crate::picker::Picker as Core;

pub(in crate::tui) use crate::picker::PickerRow;

/// picker Ctrl+D/Ctrl+U 的半页翻步长（可见行下标计）。
pub(super) const PICKER_PAGE_SCROLL: isize = 10;

/// 弹层可见行容量：超出时内核滚动窗口（贴边语义，与 CLI 选择器同一口径）。
pub(in crate::tui) const PICKER_ROW_CAPACITY: usize = 10;

/// 选择器种类：决定确认动作（[`super::Effect`]）与渲染标题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum PickerKind {
    /// `resume`：恢复历史 session
    Resume,
    /// `tree`：选择分支起点
    Tree,
    /// `models`：切换模型
    Models,
    /// 模型切换流程第二步：设置思考级别
    Reasoning,
}

/// 选择器 adapter：种类 + 选择内核（候选行/选中/滚动窗口/过滤全在内核）。
#[derive(Debug)]
pub(in crate::tui) struct Picker {
    pub(in crate::tui) kind: PickerKind,
    pub(in crate::tui) core: Core,
}

impl Picker {
    /// 打开 `resume` 选择器（从头选中）；调用方保证候选非空。
    pub(super) fn resume(rows: Vec<PickerRow>) -> Self {
        debug_assert!(!rows.is_empty());
        Self {
            kind: PickerKind::Resume,
            core: Core::new(rows),
        }
    }

    /// 打开 `models` 选择器，预选中当前模型；调用方保证候选非空。
    pub(super) fn models(rows: Vec<PickerRow>, selected: usize) -> Self {
        debug_assert!(!rows.is_empty());
        debug_assert!(selected < rows.len());
        Self {
            kind: PickerKind::Models,
            core: Core::with_selected(rows, selected),
        }
    }

    /// 打开思考级别选择器（模型切换流程第二步，预选中当前级别）；
    /// 调用方保证候选非空。
    pub(super) fn reasoning(rows: Vec<PickerRow>, selected: usize) -> Self {
        debug_assert!(!rows.is_empty());
        debug_assert!(selected < rows.len());
        Self {
            kind: PickerKind::Reasoning,
            core: Core::with_selected(rows, selected),
        }
    }

    /// 打开 `tree` 选择器（预选中 `selected`，通常是当前分支末端）；
    /// 调用方保证候选非空且 `selected` 落在可选行上。
    pub(super) fn tree(rows: Vec<PickerRow>, selected: usize) -> Self {
        debug_assert!(!rows.is_empty());
        debug_assert!(rows[selected].selectable);
        Self {
            kind: PickerKind::Tree,
            core: Core::with_selected(rows, selected),
        }
    }

    /// 过滤后的可见行（`rows` 下标列表，保持原顺序）。
    pub(in crate::tui) fn visible(&self) -> Vec<usize> {
        self.core.visible()
    }

    /// 移动选中项（跳过不可选行；弹层容量固定）。
    pub(super) fn select(&mut self, delta: isize) {
        self.core.select(delta, PICKER_ROW_CAPACITY);
    }

    /// 跳转选中到可见行的 `pos`，不可选时沿 `direction` 找可选行。
    pub(super) fn jump(&mut self, pos: usize, direction: isize) {
        self.core.jump(pos, direction, PICKER_ROW_CAPACITY);
    }

    /// 清空过滤串；返回是否确有过滤串被清空
    ///（Esc 据此决定清过滤还是关 picker）。
    pub(super) fn clear_filter(&mut self) -> bool {
        self.core.clear_filter()
    }

    /// 删除过滤串末字符（Backspace）。
    pub(super) fn pop_filter(&mut self) {
        self.core.pop_filter();
    }

    /// 可打印字符即过滤（含 j/k/q——导航全部走箭头/Ctrl 键，一键一义）。
    pub(super) fn push_filter_char(&mut self, c: char) {
        self.core.push_filter_char(c);
    }

    /// 当前选中行的（种类, id）：过滤后无可见行或选中不可选行
    ///（`tree` 的工具调用条目）时返回 `None`（不确认、保持打开）。
    pub(super) fn selected_entry(&self) -> Option<(PickerKind, String)> {
        self.core
            .selected_row()
            .map(|row| (self.kind, row.id.clone()))
    }
}
