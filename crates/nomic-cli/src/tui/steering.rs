//! 统一消息队列（TUI 交互能力，ADR-0014）。
//!
//! 运行中（含工具执行中）入队的消息，在当前 assistant turn 的工具调用
//! 执行完后、下一次 LLM 调用前，经 core 的 [`nomic_core::TurnInjection`]
//! 注入点作为 user 消息注入当前 run（pi 式转向）；run 异常结束（取消/
//! 失败）时队列保留，交互端恢复后从同一队列弹出队首作为下一轮 prompt
//! 发送（ADR-0012 的暂停保留口径）。投递口径 one-at-a-time：每个完成的
//! turn 注入一条；队列未清空时 run 不结束（模型无工具调用也注入续行）。
//!
//! 队列经共享句柄 [`SteeringQueue`] 在 agent 与交互端之间直推：agent
//! 运行期间 driver 的串行 job 通道被 prompt 占用，无法中转运行中消息，
//! 交互端持句柄克隆随时入队/编辑；core 在 turn 边界经注入点弹出。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use nomic_core::{TurnInjection, TurnMessage};

/// 共享队列句柄：agent 与交互端各持克隆，内部为同一份队列。
///
/// 全部方法可在任意时机调用（锁持有时间仅为单次队列操作，无跨 await
/// 持锁）。`Default` 即新建空队列。
#[derive(Debug, Clone, Default)]
pub(in crate::tui) struct SteeringQueue {
    inner: Arc<Mutex<VecDeque<TurnMessage>>>,
    frozen: Arc<AtomicBool>,
}

impl SteeringQueue {
    /// 入队一条消息（运行中随时可推；FIFO）。
    pub(in crate::tui) fn push(&self, message: TurnMessage) {
        self.lock().push_back(message);
    }

    /// 弹出队首：turn 边界注入与交互端暂停恢复共用；冻结期返回 `None`。
    pub(in crate::tui) fn pop_front(&self) -> Option<TurnMessage> {
        if self.is_frozen() {
            return None;
        }
        self.lock().pop_front()
    }

    /// 队列中的消息条数。
    pub(in crate::tui) fn len(&self) -> usize {
        self.lock().len()
    }

    /// 队列是否为空。
    pub(in crate::tui) fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// 清空队列（会话切换语义：排队消息是旧上下文的后续意图）。
    pub(in crate::tui) fn clear(&self) {
        self.lock().clear();
    }

    /// 冻结注入（TUI QUEUE 编辑语义）：用户手持缓冲编辑时 run 仍在
    /// 推进，不冻结会导致游标下标被 turn 边界弹出漂移；冻结期
    /// [`TurnInjection::next_message`] 返回 `None`，run 可正常结束，
    /// 队列保留。
    pub(in crate::tui) fn freeze(&self) {
        self.frozen.store(true, Ordering::Relaxed);
    }

    /// 解冻（退出 QUEUE 编辑即恢复注入）。
    pub(in crate::tui) fn unfreeze(&self) {
        self.frozen.store(false, Ordering::Relaxed);
    }

    /// 是否处于冻结期。
    pub(in crate::tui) fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::Relaxed)
    }

    /// 队列内容快照（交互端渲染用；队列短小，逐帧克隆可接受）。
    pub(in crate::tui) fn snapshot(&self) -> Vec<TurnMessage> {
        self.lock().iter().cloned().collect()
    }

    /// 删除指定下标的条目，返回被删内容；越界返回 `None`。
    pub(in crate::tui) fn remove(&self, index: usize) -> Option<TurnMessage> {
        self.lock().remove(index)
    }

    /// 交换两个下标的条目（越界为无操作）。
    pub(in crate::tui) fn swap(&self, a: usize, b: usize) {
        let mut queue = self.lock();
        if a < queue.len() && b < queue.len() {
            queue.swap(a, b);
        }
    }

    /// 在指定下标插入条目（越界收敛到队尾）。
    pub(in crate::tui) fn insert(&self, index: usize, message: TurnMessage) {
        let mut queue = self.lock();
        let index = index.min(queue.len());
        queue.insert(index, message);
    }

    /// 更新指定下标条目的文本（附件保留）；越界返回 `false`。
    pub(in crate::tui) fn update_text(&self, index: usize, text: String) -> bool {
        self.lock().get_mut(index).is_some_and(|message| {
            message.text = text;
            true
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<TurnMessage>> {
        self.inner.lock().expect("steering queue lock poisoned")
    }
}

impl TurnInjection for SteeringQueue {
    /// turn 边界注入：冻结期返回 `None`（run 可正常结束、队列保留），
    /// 否则弹出队首作为下一条注入消息。
    fn next_message(&self) -> Option<TurnMessage> {
        self.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> TurnMessage {
        TurnMessage {
            text: text.to_string(),
            images: Vec::new(),
        }
    }

    #[test]
    fn push_pop_fifo() {
        let queue = SteeringQueue::default();
        queue.push(message("a"));
        queue.push(message("b"));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop_front().expect("pop").text, "a");
        assert_eq!(queue.pop_front().expect("pop").text, "b");
        assert!(queue.pop_front().is_none());
    }

    #[test]
    fn frozen_blocks_pop_but_keeps_queue() {
        let queue = SteeringQueue::default();
        queue.push(message("a"));
        queue.freeze();
        assert!(queue.is_frozen());
        assert!(queue.pop_front().is_none());
        assert_eq!(queue.len(), 1);
        queue.unfreeze();
        assert_eq!(queue.pop_front().expect("pop").text, "a");
    }

    #[test]
    fn clones_share_the_same_queue() {
        let queue = SteeringQueue::default();
        let clone = queue.clone();
        clone.push(message("a"));
        clone.freeze();
        assert_eq!(queue.len(), 1);
        assert!(queue.is_frozen());
        assert!(queue.pop_front().is_none());
    }

    #[test]
    fn edit_operations() {
        let queue = SteeringQueue::default();
        queue.push(message("a"));
        queue.push(message("b"));
        queue.push(message("c"));
        queue.swap(0, 2);
        assert!(queue.update_text(1, "B".to_string()));
        assert!(!queue.update_text(9, "x".to_string()));
        queue.insert(1, message("inserted"));
        assert_eq!(queue.remove(0).expect("remove").text, "c");
        let texts: Vec<String> = queue.snapshot().into_iter().map(|m| m.text).collect();
        assert_eq!(texts, ["inserted", "B", "a"]);
        queue.clear();
        assert!(queue.is_empty());
    }

    #[test]
    fn turn_injection_pops_front_and_respects_freeze() {
        let queue = SteeringQueue::default();
        queue.push(message("a"));
        queue.push(message("b"));
        assert_eq!(queue.next_message().expect("inject").text, "a".to_string());
        queue.freeze();
        assert!(queue.next_message().is_none());
        queue.unfreeze();
        assert_eq!(queue.next_message().expect("inject").text, "b".to_string());
        assert!(queue.next_message().is_none());
    }
}
