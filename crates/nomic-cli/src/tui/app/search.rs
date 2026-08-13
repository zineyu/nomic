//! 聊天区搜索状态：查询串与命中集合。
//!
//! [`Search`] 自持搜索串与命中条目（NORMAL `/` 进入的增量搜索）；
//! 游标移动、模式进出与提示语由模式路由层（[`super::App`]）裁决。

use super::chat::{ChatItem, item_text};

/// 搜索状态：查询串 + 命中条目下标（升序）。
#[derive(Debug, Default)]
pub(in crate::tui) struct Search {
    /// 搜索串（NORMAL `/` 进入 SEARCH；Esc 清空，Enter 保留供 n/N）
    query: String,
    /// 搜索命中条目（items 下标，升序）
    pub(super) matches: Vec<usize>,
}

impl Search {
    /// 当前搜索串（SEARCH 输入框与命中高亮用）。
    pub(in crate::tui) fn query(&self) -> &str {
        &self.query
    }

    /// 搜索命中数（SEARCH 输入框标题用）。
    pub(in crate::tui) const fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// 命中高亮词：搜索串非空时返回（Enter 后保留高亮，Esc 清空）。
    pub(in crate::tui) fn highlight(&self) -> Option<&str> {
        (!self.query.is_empty()).then_some(self.query.as_str())
    }

    /// 追加一个查询字符（输入即搜，由调用方随后 [`Self::refresh`]）。
    pub(super) fn push_char(&mut self, c: char) {
        self.query.push(c);
    }

    /// 删除查询末字符（Backspace）。
    pub(super) fn pop_char(&mut self) {
        self.query.pop();
    }

    /// 清空查询与命中（Esc 退出搜索）。
    pub(super) fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
    }

    /// 重算搜索命中（输入即搜）：返回增量跳转目标——当前位置之后（含）
    /// 的第一个命中（循环）；无命中返回 `None`（游标保持）。
    pub(super) fn refresh(&mut self, items: &[ChatItem], current: Option<usize>) -> Option<usize> {
        let query = self.query.to_lowercase();
        self.matches = if query.is_empty() {
            Vec::new()
        } else {
            items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    item_text(item)
                        .filter(|text| text.to_lowercase().contains(&query))
                        .map(|_| index)
                })
                .collect()
        };
        if self.matches.is_empty() {
            return None;
        }
        let current = current.unwrap_or(0);
        let next = self.matches.partition_point(|&m| m < current);
        let next = if next >= self.matches.len() { 0 } else { next };
        Some(self.matches[next])
    }

    /// NORMAL `n`/`N`：在搜索命中条目间循环跳转；
    /// 返回（条目下标, 命中序号）。无命中返回 `None`。
    pub(super) fn jump(&self, direction: isize, current: usize) -> Option<(usize, usize)> {
        if self.matches.is_empty() {
            return None;
        }
        let len = self.matches.len();
        let next = if direction > 0 {
            let p = self.matches.partition_point(|&m| m <= current);
            if p >= len { 0 } else { p }
        } else {
            let p = self.matches.partition_point(|&m| m < current);
            if p == 0 { len - 1 } else { p - 1 }
        };
        Some((self.matches[next], next))
    }
}
