//! 统一消息队列与 QUEUE 模式状态（ADR-0012/0014）。
//!
//! [`Queue`] 自持与 agent 共享的 [`SteeringQueue`]、QUEUE 模式的条目
//! 游标与就地编辑子状态；冻结/解冻、入队/drain 的时机与效果分发由
//! [`super::App`] 路由。

use std::sync::Arc;

use nomic_core::{TurnInjection, TurnMessage};

use super::{line_count_of, step_row};
use crate::tui::steering::SteeringQueue;

/// 队列区渲染条目（统一队列视图，与 QUEUE 模式游标的条目空间一致）。
#[derive(Debug)]
pub(in crate::tui) struct QueueEntry {
    pub(in crate::tui) text: String,
    pub(in crate::tui) images: usize,
}

/// 队列状态：共享队列 + QUEUE 模式游标 + 就地编辑槽位。
/// 键位语义（导航/删除/换位/编辑）实现为状态上的原子操作，
/// 模式进出与提示语由模式路由层裁决。
#[derive(Debug, Default)]
pub(in crate::tui) struct Queue {
    /// 统一消息队列（ADR-0014）：与 agent 共享同一份（启动时经
    /// [`Self::handle`] 传给 builder）；QUEUE 模式打开期间冻结注入
    steering: SteeringQueue,
    /// QUEUE 模式的条目游标（队列下标）
    pub(super) cursor: usize,
    /// QUEUE 模式的编辑子状态：正在就地编辑的队列槽位
    ///（草稿缓冲即该槽位内容，Enter/Esc 保存回队列）
    editing: Option<usize>,
}

impl Queue {
    /// 统一消息队列句柄（ADR-0014）：与 agent 共享同一份队列，并作为
    /// core 的注入源（[`TurnInjection`]）在 turn 边界弹出队首；启动时经
    /// builder 的 `turn_injection` 传入。
    pub(in crate::tui) fn handle(&self) -> Arc<dyn TurnInjection> {
        Arc::new(self.steering.clone())
    }

    /// 入队（ADR-0014）：当前 turn 的工具调用执行完后由 core 在 turn
    /// 边界经注入点注入本轮运行（run 异常结束时保留，恢复后作为下一轮
    /// prompt）。
    pub(super) fn push(&self, message: TurnMessage) {
        self.steering.push(message);
    }

    /// 取出队首（drain 恢复路径；QUEUE 模式未打开才由调用方发起）。
    pub(super) fn pop_front(&self) -> Option<TurnMessage> {
        self.steering.pop_front()
    }

    /// 清空队列（`/new`、`/resume`、`/tree` 切换上下文时随旧对话意图丢弃）。
    pub(super) fn clear(&self) {
        self.steering.clear();
    }

    /// 冻结队列注入（进入 QUEUE 模式）：用户手持缓冲编辑时 run 仍在
    /// 推进，不冻结会让 core 在 turn 边界弹走条目导致游标下标漂移。
    pub(super) fn freeze(&self) {
        self.steering.freeze();
    }

    /// 解冻队列注入（退出 QUEUE 模式）。
    pub(super) fn unfreeze(&self) {
        self.steering.unfreeze();
    }

    /// 是否处于冻结期（进入 QUEUE 编辑时冻结注入；测试用）。
    #[cfg(test)]
    pub(in crate::tui) fn is_frozen(&self) -> bool {
        self.steering.is_frozen()
    }

    /// 排队消息总条数（运行中标题与暂停提示用）。
    pub(in crate::tui) fn len(&self) -> usize {
        self.steering.len()
    }

    /// 队列是否为空（进入 QUEUE 模式的守卫用）。
    pub(in crate::tui) fn is_empty(&self) -> bool {
        self.steering.is_empty()
    }

    /// 队列条目视图（输入框队列区渲染用），与 QUEUE 模式游标的
    /// 条目空间一致。
    pub(in crate::tui) fn entries(&self) -> Vec<QueueEntry> {
        self.steering
            .snapshot()
            .into_iter()
            .map(|m| QueueEntry {
                text: m.text,
                images: m.images.len(),
            })
            .collect()
    }

    /// QUEUE 条目游标（渲染高亮用；仅 QUEUE 模式下有意义）。
    pub(in crate::tui) const fn cursor(&self) -> usize {
        self.cursor
    }

    /// 进入 QUEUE 模式时复位游标与编辑子状态。
    pub(super) const fn reset(&mut self) {
        self.cursor = 0;
        self.editing = None;
    }

    /// QUEUE `j`/`k`：移动条目游标（钳制不循环）。
    pub(super) fn move_cursor(&mut self, delta: isize) {
        if let Some(next) = step_row(self.cursor, delta, self.steering.len()) {
            self.cursor = next;
        }
    }

    /// QUEUE `gg`：跳到队首。
    pub(super) const fn jump_to_first(&mut self) {
        self.cursor = 0;
    }

    /// QUEUE `G`：跳到队尾。
    pub(super) fn jump_to_last(&mut self) {
        self.cursor = self.steering.len().saturating_sub(1);
    }

    /// QUEUE `dd`/`x`：删除游标条目（oil.nvim 删行语义）；
    /// 返回队列是否被清空（调用方据此退出 QUEUE 并提示）。
    pub(super) fn delete(&mut self) -> bool {
        if self.steering.is_empty() {
            return false;
        }
        self.steering.remove(self.cursor);
        if self.steering.is_empty() {
            return true;
        }
        self.cursor = self.cursor.min(self.steering.len() - 1);
        false
    }

    /// QUEUE `J`/`K`：游标条目与下/上一条换位（vim `:move` 语义，
    /// 到底/顶不动）。
    pub(super) fn swap(&mut self, delta: isize) {
        let Some(next) = step_row(self.cursor, delta, self.steering.len()) else {
            return;
        };
        self.steering.swap(self.cursor, next);
        self.cursor = next;
    }

    /// 游标槽位的文本（QUEUE `i`/`a`/Enter 就地编辑载入草稿用）。
    pub(super) fn current_slot_text(&self) -> Option<String> {
        self.steering
            .snapshot()
            .get(self.cursor)
            .map(|m| m.text.clone())
    }

    /// QUEUE `o`/`O`：在游标下/上方插入空槽位（保存空文本即撤销该槽位，
    /// 与保存语义一致）。
    pub(super) fn insert_slot(&mut self, offset: usize) {
        let index = self.cursor + offset;
        self.steering.insert(
            index,
            TurnMessage {
                text: String::new(),
                images: Vec::new(),
            },
        );
        self.cursor = index;
    }

    /// 保存就地编辑：写回槽位；空文本删除槽位（oil.nvim 空行忽略
    /// 语义）；返回队列是否被清空（调用方据此退出 QUEUE 并提示）。
    pub(super) fn save_edit(&mut self, slot: usize, text: String) -> bool {
        if text.is_empty() {
            self.steering.remove(slot);
        } else {
            self.steering.update_text(slot, text);
        }
        if self.steering.is_empty() {
            return true;
        }
        self.cursor = slot.min(self.steering.len() - 1);
        false
    }

    /// QUEUE 是否处于编辑子状态（光标形状与状态栏提示用）。
    pub(in crate::tui) const fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// 就地编辑的槽位下标（渲染时用草稿行替换该槽位内容）。
    pub(in crate::tui) const fn editing_slot(&self) -> Option<usize> {
        self.editing
    }

    /// 开始就地编辑游标槽位（附件保留在槽位上，不随文本进缓冲）。
    pub(super) const fn begin_edit(&mut self) {
        self.editing = Some(self.cursor);
    }

    /// 结束就地编辑，返回被编辑的槽位（保存写回用）。
    pub(super) const fn take_editing(&mut self) -> Option<usize> {
        self.editing.take()
    }

    /// 放弃编辑子状态（退出 QUEUE 模式时复位用）。
    pub(super) const fn end_edit(&mut self) {
        self.editing = None;
    }

    /// QUEUE 游标条目的起始展示行（队列区内，不含附件行）：
    /// 渲染光标定位用；就地编辑时另加草稿缓冲内的光标行。
    pub(in crate::tui) fn cursor_row(&self) -> u16 {
        let mut row = 0_u16;
        for (index, entry) in self.entries().iter().enumerate() {
            if index == self.cursor {
                break;
            }
            row = row.saturating_add(line_count_of(&entry.text));
        }
        row
    }
}
