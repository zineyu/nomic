//! `ask_user_question` 的 TUI 交互端（ADR-0029）：工具侧 → 事件循环。
//!
//! 工具在 agent actor 任务内执行，事件循环在主任务渲染 TUI——两者间用
//! 一条独立 mpsc 通道直连：工具侧 [`TuiQuestionSink::ask`] 把问题连同
//! `oneshot::Sender` 推入通道并阻塞等待回答；事件循环收到
//! `Wake::UserQuestion`（driver.rs）后在状态层打开提问弹层
//!（`super::app::question`），用户作答后经 oneshot 把回答送回工具。
//!
//! 取消语义：`cancel` 触发（用户 NORMAL `q` / Ctrl+C 中断运行）或事件
//! 循环侧放弃问题（Esc 取消 / 运行结束）时，接收端先就绪即返回错误，
//! 工具不挂起。执行模式 `ExecutionMode::Sequential`（nomic_core）保证
//! 同批次至多一个提问在途，通道不会积压第二个未答问题。

use async_trait::async_trait;
use nomic_core::ToolError;
use nomic_tools::{AskUserAnswer, AskUserQuestion, QuestionSink};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// 在途问题：问题描述 + 回答回传通道（事件循环侧持有发送端，
/// 用户作答 / 取消 / 运行结束时发送或丢弃）。
pub(in crate::tui) struct PendingQuestion {
    pub(in crate::tui) question: AskUserQuestion,
    pub(in crate::tui) answer_tx: oneshot::Sender<AskUserAnswer>,
}

/// [`QuestionSink`] 的 TUI 实现：把问题推给事件循环并等待回答。
pub(in crate::tui) struct TuiQuestionSink {
    tx: mpsc::UnboundedSender<PendingQuestion>,
}

impl TuiQuestionSink {
    /// 绑定到事件循环的提问通道（[`super::run`] 创建）。
    pub(in crate::tui) const fn new(tx: mpsc::UnboundedSender<PendingQuestion>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl QuestionSink for TuiQuestionSink {
    async fn ask(
        &self,
        question: AskUserQuestion,
        cancel: CancellationToken,
    ) -> Result<AskUserAnswer, ToolError> {
        let (answer_tx, answer_rx) = oneshot::channel();
        self.tx
            .send(PendingQuestion {
                question,
                answer_tx,
            })
            .map_err(|_| ToolError::new("TUI exited; cannot ask the user"))?;
        tokio::select! {
            answer = answer_rx => {
                answer.map_err(|_| ToolError::new("question cancelled (no answer received)"))
            }
            () = cancel.cancelled() => {
                Err(ToolError::new("question cancelled (run aborted)"))
            }
        }
    }
}
