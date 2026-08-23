//! web 侧统一消息队列（steering，ADR-0014 语义；队列落点对齐 ADR-0027：
//! core 只保留 [`TurnInjection`] 注入点，队列的存储与编辑是交互端能力）。
//!
//! 运行中提交的 prompt 进入本队列，core 在每个完成的 turn 边界经
//! [`TurnInjection::next_message`] 弹出队首注入本轮运行（one-at-a-time，
//! 队列未清空 run 不结束）；run 异常结束（取消/失败）时队列保留，正常
//! 结束时的滞后入队（注入点查询后的竞态窗口）由 runner 事件侧的 drain
//! 弹出队首作为下一轮 prompt（见 `session::forward_runner_events`）。
//!
//! 与 TUI 的差异：
//! - TUI 以游标下标编辑、QUEUE 模式打开期间冻结注入防下标漂移；web 的
//!   编辑操作来自远程客户端、与 turn 边界弹出天然并发，故条目以**稳定
//!   id** 寻址（编辑/删除/换位均按 id），无需冻结机制；
//! - 队列存用户输入原文（展示与编辑的对象），`@skill:` / `@file:`
//!   mention 在**投递时**（弹出队首）展开——注入与 drain 两条消费路径
//!   同一口径，且展开内容以投递时刻为准。
//!
//! 每次变更（入队 / 弹出 / 编辑 / 删除 / 换位）后向全局事件总线广播
//! 全量快照 `queue_changed`：队列短小，全量推送换取前端零增量合并逻辑
//! 与并发下的最终一致（后发全量覆盖先发）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nomic_ai::ImageContent;
use nomic_core::{TurnInjection, TurnMessage};
use serde::Serialize;
use tokio::sync::broadcast;

use super::ServerEvent;

/// 队列条目的协议视图（快照与 `queue_changed` 事件携带）：稳定 id +
/// 原文文本 + 附件数（图片内容不回传前端，编辑仅改文本，附件保留在
/// 槽位上）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueueEntryView {
    /// 条目 id（session 内单调递增，编辑寻址用）
    pub id: String,
    /// 消息原文（mention 未展开）
    pub text: String,
    /// 图片附件数
    pub images: usize,
}

/// 队列条目（存储）：id + 原文 + 图片附件。
#[derive(Debug, Clone)]
struct Entry {
    id: u64,
    text: String,
    images: Vec<ImageContent>,
}

/// mention 展开器：投递时把原文中的 `@skill:` / `@file:` 展开为内容
/// （`None` 为恒等，测试与非交互入口用）。
type MentionExpander = Option<Arc<dyn Fn(&str) -> String + Send + Sync>>;

/// web 统一消息队列：agent（注入点）与 WebSocket handler（入队/编辑）
/// 各持克隆，内部为同一份队列。
///
/// 全部方法可在任意时机调用（锁持有时间仅为单次队列操作，无跨 await
/// 持锁）；变更通知在锁外广播。
#[derive(Clone)]
pub struct MessageQueue {
    inner: Arc<Mutex<VecDeque<Entry>>>,
    next_id: Arc<AtomicU64>,
    session_id: String,
    events: broadcast::Sender<ServerEvent>,
    expand: MentionExpander,
}

impl std::fmt::Debug for MessageQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageQueue")
            .field("session_id", &self.session_id)
            .field("len", &self.lock().len())
            .finish_non_exhaustive()
    }
}

impl MessageQueue {
    /// 新建空队列：绑定本 session 的 id 与全局事件总线（变更广播用）；
    /// `expand` 为投递时的 mention 展开器（`None` 不展开）。
    pub fn new(
        session_id: String,
        events: broadcast::Sender<ServerEvent>,
        expand: MentionExpander,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            session_id,
            events,
            expand,
        }
    }

    /// 入队一条消息（运行中随时可推；FIFO），返回条目 id。
    pub fn push(&self, text: String, images: Vec<ImageContent>) -> String {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock().push_back(Entry { id, text, images });
        self.notify();
        id.to_string()
    }

    /// 弹出队首（mention 在投递时展开）：turn 边界注入与 drain 恢复共用。
    pub fn pop_front(&self) -> Option<TurnMessage> {
        let entry = self.lock().pop_front();
        entry.map(|entry| {
            self.notify();
            self.materialize(entry)
        })
    }

    /// 队列内容快照（会话快照与变更广播用；队列短小，逐次克隆可接受）。
    pub fn snapshot(&self) -> Vec<QueueEntryView> {
        self.lock()
            .iter()
            .map(|entry| QueueEntryView {
                id: entry.id.to_string(),
                text: entry.text.clone(),
                images: entry.images.len(),
            })
            .collect()
    }

    /// 更新条目原文（附件保留）；id 不存在返回 `false`。
    pub fn update_text(&self, id: &str, text: String) -> bool {
        let updated = self
            .lock()
            .iter_mut()
            .find(|entry| entry.id.to_string() == id)
            .is_some_and(|entry| {
                entry.text = text;
                true
            });
        if updated {
            self.notify();
        }
        updated
    }

    /// 删除条目；id 不存在返回 `false`。
    pub fn remove(&self, id: &str) -> bool {
        let removed = {
            let mut queue = self.lock();
            queue
                .iter()
                .position(|entry| entry.id.to_string() == id)
                .and_then(|index| queue.remove(index))
                .is_some()
        };
        if removed {
            self.notify();
        }
        removed
    }

    /// 条目上移/下移一位（到底/顶不动）；id 不存在返回 `false`。
    pub fn move_by(&self, id: &str, delta: isize) -> bool {
        let mut queue = self.lock();
        let moved = match queue.iter().position(|entry| entry.id.to_string() == id) {
            Some(index) => match index.checked_add_signed(delta) {
                Some(next) if next < queue.len() => {
                    queue.swap(index, next);
                    true
                }
                _ => false,
            },
            None => false,
        };
        drop(queue);
        if moved {
            self.notify();
        }
        moved
    }

    /// 变更广播：向全局事件总线推送全量队列快照（无订阅者时静默丢弃）。
    fn notify(&self) {
        let _ = self.events.send(ServerEvent::QueueChanged {
            session_id: self.session_id.clone(),
            queue: self.snapshot(),
        });
    }

    /// 条目落为投递消息：mention 展开器作用于原文（无展开器即恒等）。
    fn materialize(&self, entry: Entry) -> TurnMessage {
        let text = match &self.expand {
            Some(expand) => expand(&entry.text),
            None => entry.text,
        };
        TurnMessage {
            text,
            images: entry.images,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Entry>> {
        self.inner.lock().expect("message queue lock poisoned")
    }
}

impl TurnInjection for MessageQueue {
    /// turn 边界注入：弹出队首（mention 已展开）作为下一条注入消息
    ///（web 无冻结语义：编辑按稳定 id 寻址，不受弹出导致的下标漂移影响）。
    fn next_message(&self) -> Option<TurnMessage> {
        self.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> (MessageQueue, broadcast::Receiver<ServerEvent>) {
        let (events, rx) = broadcast::channel(16);
        (MessageQueue::new("s1".to_string(), events, None), rx)
    }

    fn texts(queue: &MessageQueue) -> Vec<String> {
        queue
            .snapshot()
            .into_iter()
            .map(|entry| entry.text)
            .collect()
    }

    /// 取通知事件中的队列快照。
    async fn recv_queue(rx: &mut broadcast::Receiver<ServerEvent>) -> Vec<QueueEntryView> {
        match rx.recv().await.expect("queue_changed event") {
            ServerEvent::QueueChanged { queue, .. } => queue,
            other => panic!("应广播 QueueChanged，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn push_pop_fifo_and_notify() {
        let (queue, mut rx) = queue();
        queue.push("a".to_string(), Vec::new());
        queue.push("b".to_string(), Vec::new());
        assert_eq!(recv_queue(&mut rx).await.len(), 1);
        assert_eq!(recv_queue(&mut rx).await.len(), 2);
        assert_eq!(queue.snapshot().len(), 2);

        assert_eq!(queue.pop_front().expect("pop").text, "a");
        assert_eq!(recv_queue(&mut rx).await.len(), 1);
        assert_eq!(queue.pop_front().expect("pop").text, "b");
        assert!(queue.pop_front().is_none());
        assert!(queue.snapshot().is_empty());
    }

    #[test]
    fn clones_share_the_same_queue() {
        let (queue, _rx) = queue();
        let clone = queue.clone();
        clone.push("a".to_string(), Vec::new());
        assert_eq!(queue.snapshot().len(), 1);
        assert_eq!(queue.pop_front().expect("pop").text, "a");
    }

    #[tokio::test]
    async fn edit_operations_by_stable_id() {
        let (queue, _rx) = queue();
        let a = queue.push("a".to_string(), Vec::new());
        let b = queue.push("b".to_string(), Vec::new());
        let c = queue.push("c".to_string(), Vec::new());

        // 更新文本（附件保留）
        assert!(queue.update_text(&b, "B".to_string()));
        assert!(!queue.update_text("missing", "x".to_string()));
        assert_eq!(texts(&queue), ["a", "B", "c"]);

        // 换位：到底/顶不动，id 寻址不受其他条目增删影响
        assert!(queue.move_by(&c, -1));
        assert_eq!(texts(&queue), ["a", "c", "B"]);
        assert!(!queue.move_by(&a, -1), "队首上移应不动");
        assert!(!queue.move_by("missing", 1));
        assert_eq!(texts(&queue), ["a", "c", "B"]);

        // 弹出队首后编辑仍命中（稳定 id 不受下标漂移影响）
        assert_eq!(queue.pop_front().expect("pop").text, "a");
        assert!(queue.update_text(&c, "C".to_string()));
        assert_eq!(texts(&queue), ["C", "B"]);

        // 删除
        assert!(queue.remove(&b));
        assert!(!queue.remove(&b), "重复删除应失败");
        assert_eq!(texts(&queue), ["C"]);
        assert_eq!(queue.snapshot().len(), 1);
    }

    #[test]
    fn turn_injection_pops_front() {
        let (queue, _rx) = queue();
        queue.push("a".to_string(), Vec::new());
        queue.push("b".to_string(), Vec::new());
        assert_eq!(queue.next_message().expect("inject").text, "a");
        assert_eq!(queue.next_message().expect("inject").text, "b");
        assert!(queue.next_message().is_none());
    }

    #[test]
    fn ids_are_unique_across_pop_and_push() {
        let (queue, _rx) = queue();
        let first = queue.push("a".to_string(), Vec::new());
        queue.pop_front();
        let second = queue.push("b".to_string(), Vec::new());
        assert_ne!(first, second, "弹出后再入队的 id 不应复用");
    }

    #[test]
    fn expander_applies_at_delivery_not_at_push() {
        let (events, _rx) = broadcast::channel(16);
        let queue = MessageQueue::new(
            "s1".to_string(),
            events,
            Some(Arc::new(|text: &str| text.replace("@file:x", "<内容>"))),
        );
        queue.push("@file:x 摘要".to_string(), Vec::new());
        // 快照保留原文（展示/编辑对象）
        assert_eq!(texts(&queue), ["@file:x 摘要"]);
        // 弹出时展开
        assert_eq!(queue.pop_front().expect("pop").text, "<内容> 摘要");
    }
}
