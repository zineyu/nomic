//! steering 队列（pi 式运行中转向，ADR-0013）。
//!
//! 运行中（含工具执行中）入队的转向消息，在当前 assistant turn 的工具
//! 调用执行完后、下一次 LLM 调用前，作为 user 消息注入当前 run——
//! 与 follow-up（本轮结束后发送）不同，steering 用于「看到走偏立即
//! 纠偏」。投递口径 one-at-a-time：每个完成的 turn 注入一条；队列未
//! 清空时 run 不结束（模型无工具调用也注入续行）。
//!
//! 队列经共享句柄 [`SteeringQueue`] 在 agent 与交互端之间直推：agent
//! 运行期间 driver 的串行 job 通道被 prompt 占用，无法中转运行中消息，
//! 交互端持句柄克隆随时入队/编辑，agent 在 turn 边界弹出。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use nomic_ai::ImageContent;

/// 一条待注入的 steering 消息（运行中由交互端入队，turn 边界注入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringMessage {
    /// 消息文本
    pub text: String,
    /// 图片附件（与 prompt 附件同一口径：图片块在前、文本块在后）
    pub images: Vec<ImageContent>,
}

/// 共享 steering 队列句柄：agent 与交互端各持克隆，内部为同一份队列。
///
/// 全部方法可在任意时机调用（锁持有时间仅为单次队列操作，无跨 await
/// 持锁）。`Default` 即新建空队列。
#[derive(Debug, Clone, Default)]
pub struct SteeringQueue {
    inner: Arc<Mutex<VecDeque<SteeringMessage>>>,
    frozen: Arc<AtomicBool>,
}

impl SteeringQueue {
    /// 新建空队列。
    pub fn new() -> Self {
        Self::default()
    }

    /// 入队一条 steering 消息（运行中随时可推；FIFO）。
    pub fn push(&self, message: SteeringMessage) {
        self.lock().push_back(message);
    }

    /// 弹出队首：turn 边界注入与交互端暂停恢复共用；冻结期返回 `None`。
    pub fn pop_front(&self) -> Option<SteeringMessage> {
        if self.is_frozen() {
            return None;
        }
        self.lock().pop_front()
    }

    /// 队列中的消息条数。
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// 清空队列（会话切换语义：排队消息是旧上下文的后续意图）。
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// 冻结注入（TUI QUEUE 编辑语义）：用户手持缓冲编辑时 run 仍在
    /// 推进，不冻结会导致游标下标被 turn 边界弹出漂移；冻结期
    /// [`Self::pop_front`] 返回 `None`，run 可正常结束，队列保留。
    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::Relaxed);
    }

    /// 解冻（退出 QUEUE 编辑即恢复注入）。
    pub fn unfreeze(&self) {
        self.frozen.store(false, Ordering::Relaxed);
    }

    /// 是否处于冻结期。
    pub fn is_frozen(&self) -> bool {
        self.frozen.load(Ordering::Relaxed)
    }

    /// 队列内容快照（交互端渲染用；队列短小，逐帧克隆可接受）。
    pub fn snapshot(&self) -> Vec<SteeringMessage> {
        self.lock().iter().cloned().collect()
    }

    /// 删除指定下标的条目，返回被删内容；越界返回 `None`。
    pub fn remove(&self, index: usize) -> Option<SteeringMessage> {
        self.lock().remove(index)
    }

    /// 交换两个下标的条目（越界为无操作）。
    pub fn swap(&self, a: usize, b: usize) {
        let mut queue = self.lock();
        if a < queue.len() && b < queue.len() {
            queue.swap(a, b);
        }
    }

    /// 在指定下标插入条目（越界收敛到队尾）。
    pub fn insert(&self, index: usize, message: SteeringMessage) {
        let mut queue = self.lock();
        let index = index.min(queue.len());
        queue.insert(index, message);
    }

    /// 更新指定下标条目的文本（附件保留）；越界返回 `false`。
    pub fn update_text(&self, index: usize, text: String) -> bool {
        self.lock().get_mut(index).is_some_and(|message| {
            message.text = text;
            true
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<SteeringMessage>> {
        self.inner.lock().expect("steering queue lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> SteeringMessage {
        SteeringMessage {
            text: text.to_string(),
            images: Vec::new(),
        }
    }

    #[test]
    fn push_pop_fifo() {
        let queue = SteeringQueue::new();
        queue.push(message("a"));
        queue.push(message("b"));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop_front().expect("pop").text, "a");
        assert_eq!(queue.pop_front().expect("pop").text, "b");
        assert!(queue.pop_front().is_none());
    }

    #[test]
    fn frozen_blocks_pop_but_keeps_queue() {
        let queue = SteeringQueue::new();
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
        let queue = SteeringQueue::new();
        let clone = queue.clone();
        clone.push(message("a"));
        clone.freeze();
        assert_eq!(queue.len(), 1);
        assert!(queue.is_frozen());
        assert!(queue.pop_front().is_none());
    }

    #[test]
    fn edit_operations() {
        let queue = SteeringQueue::new();
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
}
