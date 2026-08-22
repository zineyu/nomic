//! 在途提问注册表：[`QuestionSink`] 宿主侧共享的提问生命周期（ADR-0029）。
//!
//! 工具经 [`QuestionSink::ask`] 把问题交给宿主后，「登记 → 等待回答 →
//! 应答回填 / 取消丢弃 → 当前快照」这套生命周期各前端（TUI 弹层、web
//! 事件总线）原本各自实现、取消语义开始分叉；收在本模块统一。UI 呈现
//!（TUI 弹层状态机、web 的 WebSocket 广播与断线重放）仍留各 adapter：
//! adapter 从 [`register`](QuestionRegistry::register) 拿到问题 id 与内容
//! 自行展示，回答 / 取消时回调注册表。
//!
//! 取消语义唯一口径：无论取消来自工具侧 cancel 令牌还是 UI 侧放弃，都是
//! [`discard`](QuestionRegistry::discard) 移除条目——回答通道随条目 drop
//! 关闭，等待中的 sink 收到通道关闭转为错误结果回喂模型。两个方向重复
//! 丢弃是幂等的（返回是否确有条目被丢弃）。

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::{AskUserAnswer, AskUserQuestion};

/// 在途提问注册表（问题 id → 条目）。clone 不共享——跨组件共享用 `Arc`。
#[derive(Debug, Default)]
pub struct QuestionRegistry {
    entries: Mutex<HashMap<String, Entry>>,
}

/// 一条在途提问：问题内容供快照重放，回答通道在应答时回填给工具。
#[derive(Debug)]
struct Entry {
    question: AskUserQuestion,
    answer_tx: oneshot::Sender<AskUserAnswer>,
}

impl QuestionRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个在途提问：返回问题 id 与回答接收端。sink 持有接收端阻塞
    /// 等待；id 与问题由 adapter 交给 UI 展示（应答 / 丢弃凭 id 回调）。
    pub fn register(
        &self,
        question: AskUserQuestion,
    ) -> (String, oneshot::Receiver<AskUserAnswer>) {
        let id = uuid::Uuid::now_v7().to_string();
        tracing::debug!(id = %id, question = %question.question, kind = ?question.kind, "question registered");
        let (answer_tx, answer_rx) = oneshot::channel();
        self.entries
            .lock()
            .expect("question registry lock poisoned")
            .insert(
                id.clone(),
                Entry {
                    question,
                    answer_tx,
                },
            );
        (id, answer_rx)
    }

    /// 应答回填：移除条目并回传回答；提问不存在（已回答 / 已丢弃）或
    /// 等待方已退出返回 `false`。
    pub fn answer(&self, id: &str, answer: AskUserAnswer) -> bool {
        let Some(entry) = self
            .entries
            .lock()
            .expect("question registry lock poisoned")
            .remove(id)
        else {
            tracing::debug!(id = %id, "question answer: not found (already answered or discarded)");
            return false;
        };
        tracing::debug!(id = %id, answers = answer.answers.len(), "question answered");
        entry.answer_tx.send(answer).is_ok()
    }

    /// 取消丢弃：移除条目（回答通道随条目 drop 关闭，等待中的 sink 收到
    /// 通道关闭转为错误结果）。返回是否确有在途条目被丢弃——web 据此决定
    /// 是否广播取消事件（前端收起弹层）。
    pub fn discard(&self, id: &str) -> bool {
        tracing::debug!(id = %id, "question discarded");
        self.entries
            .lock()
            .expect("question registry lock poisoned")
            .remove(id)
            .is_some()
    }

    /// 当前在途提问快照（web 断线重连后重放弹层）。工具声明
    /// `ExecutionMode::Sequential` 保证同批次至多一个提问在途，取任意一条。
    pub fn current(&self) -> Option<(String, AskUserQuestion)> {
        self.entries
            .lock()
            .expect("question registry lock poisoned")
            .iter()
            .next()
            .map(|(id, entry)| (id.clone(), entry.question.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(text: &str) -> AskUserQuestion {
        AskUserQuestion {
            question: text.to_string(),
            kind: crate::QuestionKind::SingleChoice,
            options: vec!["是".to_string(), "否".to_string()],
        }
    }

    fn answer(text: &str) -> AskUserAnswer {
        AskUserAnswer {
            answers: vec![text.to_string()],
            custom: None,
        }
    }

    #[tokio::test]
    async fn register_then_answer_roundtrip() {
        let registry = QuestionRegistry::new();
        let (id, rx) = registry.register(question("继续？"));
        assert!(registry.answer(&id, answer("是")), "应答应成功");
        assert_eq!(rx.await.expect("answer"), answer("是"));
        assert!(!registry.answer(&id, answer("否")), "重复应答应失败");
        assert!(registry.current().is_none(), "应答后无在途提问");
    }

    #[tokio::test]
    async fn discard_closes_answer_channel() {
        let registry = QuestionRegistry::new();
        let (id, rx) = registry.register(question("继续？"));
        assert!(registry.discard(&id), "丢弃在途条目应成功");
        assert!(!registry.discard(&id), "重复丢弃幂等");
        assert!(
            rx.await.is_err(),
            "丢弃后等待方应收到通道关闭（转为错误结果）"
        );
    }

    #[test]
    fn answer_and_discard_missing_question_return_false() {
        let registry = QuestionRegistry::new();
        assert!(!registry.answer("missing", answer("是")));
        assert!(!registry.discard("missing"));
    }

    #[test]
    fn current_snapshots_pending_question() {
        let registry = QuestionRegistry::new();
        assert!(registry.current().is_none());
        let (id, _rx) = registry.register(question("继续？"));
        let (snap_id, snap_question) = registry.current().expect("在途提问快照");
        assert_eq!(snap_id, id);
        assert_eq!(snap_question, question("继续？"));
    }
}
