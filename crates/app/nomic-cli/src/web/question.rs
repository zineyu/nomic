//! `ask_user_question` 工具的 web 实现：问题经 SSE 推给前端弹层，回答经
//! REST（`POST /api/question/{id}`）回填 oneshot，再回喂模型。
//!
//! 与 print 模式的 stdin、TUI 的弹层同一契约（[`QuestionSink`]）；`cancel`
//! 触发时移除应答表并广播取消事件（前端收起弹层），立即返回错误结束工具。

use std::sync::Arc;

use async_trait::async_trait;
use nomic_core::ToolError;
use nomic_tools::{AskUserAnswer, AskUserQuestion, QuestionSink};
use tokio_util::sync::CancellationToken;

use crate::web::{PendingQuestion, Runtime, ServerEvent};

/// web 模式的问题宿：问题广播到 SSE，等待前端回答。
#[derive(Debug)]
pub struct WebQuestionSink {
    pub runtime: Arc<Runtime>,
}

#[async_trait]
impl QuestionSink for WebQuestionSink {
    async fn ask(
        &self,
        question: AskUserQuestion,
        cancel: CancellationToken,
    ) -> Result<AskUserAnswer, ToolError> {
        let id = uuid::Uuid::now_v7().to_string();
        let (answer_tx, answer_rx) = tokio::sync::oneshot::channel();
        self.runtime.questions.lock().await.insert(
            id.clone(),
            PendingQuestion {
                question: question.clone(),
                answer: answer_tx,
            },
        );
        let _ = self.runtime.events.send(ServerEvent::Question {
            id: id.clone(),
            question,
        });

        tokio::select! {
            () = cancel.cancelled() => {
                // 运行被中断：移除应答表（若尚未被回答）并通知前端收起弹层
                if self.runtime.questions.lock().await.remove(&id).is_some() {
                    let _ = self.runtime.events.send(ServerEvent::QuestionCancelled { id });
                }
                Err(ToolError::new("question cancelled (run aborted)"))
            }
            answer = answer_rx => {
                // 回答已消费：应答表条目随 remove 一起丢弃
                self.runtime.questions.lock().await.remove(&id);
                answer.map_err(|_| ToolError::new("question answer channel closed"))
            }
        }
    }
}
