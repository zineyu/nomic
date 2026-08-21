//! `ask_user_question` 的 TUI 交互端（ADR-0029）：工具侧 → 事件循环。
//!
//! 工具在 agent actor 任务内执行，事件循环在主任务渲染 TUI——两者间用
//! 一条独立 mpsc 通道直连：工具侧 [`TuiQuestionSink::ask`] 先在共享
//! [`QuestionRegistry`] 登记（拿到问题 id 与回答接收端），把 id 连同
//! 问题推入通道并阻塞等待回答；事件循环收到 `Wake::UserQuestion`
//!（driver.rs）后在状态层打开提问弹层（`super::app::question`），用户
//! 作答 / 取消时凭 id 回调注册表（应答回填 / 丢弃，通道关闭即解除阻塞）。
//!
//! 取消语义收在注册表（唯一口径，与 web 一致）：`cancel` 触发（用户
//! NORMAL `q` / Ctrl+C 中断运行）或事件循环侧放弃问题（Esc 取消 / 运行
//! 结束）时条目被丢弃、回答通道关闭，接收端先就绪即返回错误，工具不挂起。
//! 执行模式 `ExecutionMode::Sequential`（nomic_core）保证同批次至多一个
//! 提问在途，通道不会积压第二个未答问题。

use std::sync::Arc;

use async_trait::async_trait;
use nomic_core::ToolError;
use nomic_tools::{AskUserAnswer, AskUserQuestion, QuestionRegistry, QuestionSink};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// 在途问题（注册表条目的 UI 侧句柄）：问题 id（应答 / 丢弃凭它回调
/// 注册表）+ 问题内容（弹层展示）。
pub(in crate::tui) struct PendingQuestion {
    pub(in crate::tui) id: String,
    pub(in crate::tui) question: AskUserQuestion,
}

/// [`QuestionSink`] 的 TUI 实现：登记到共享注册表，把问题推给事件循环
/// 并等待回答（回答 / 取消都经注册表回调，通道关闭即解除阻塞）。
pub(in crate::tui) struct TuiQuestionSink {
    registry: Arc<QuestionRegistry>,
    tx: mpsc::UnboundedSender<PendingQuestion>,
}

impl TuiQuestionSink {
    /// 绑定共享注册表与事件循环的提问通道（[`super::run`] 创建）。
    pub(in crate::tui) const fn new(
        registry: Arc<QuestionRegistry>,
        tx: mpsc::UnboundedSender<PendingQuestion>,
    ) -> Self {
        Self { registry, tx }
    }
}

#[async_trait]
impl QuestionSink for TuiQuestionSink {
    async fn ask(
        &self,
        question: AskUserQuestion,
        cancel: CancellationToken,
    ) -> Result<AskUserAnswer, ToolError> {
        let (id, answer_rx) = self.registry.register(question.clone());
        if self
            .tx
            .send(PendingQuestion {
                id: id.clone(),
                question,
            })
            .is_err()
        {
            self.registry.discard(&id);
            return Err(ToolError::new("TUI exited; cannot ask the user"));
        }
        tokio::select! {
            answer = answer_rx => {
                answer.map_err(|_| ToolError::new("question cancelled (no answer received)"))
            }
            () = cancel.cancelled() => {
                // 运行被中断：丢弃注册表条目（事件循环侧的重复丢弃幂等）
                self.registry.discard(&id);
                Err(ToolError::new("question cancelled (run aborted)"))
            }
        }
    }
}
