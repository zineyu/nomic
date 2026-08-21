//! `ask_user_question` 工具的 web 实现：问题经 WebSocket 推给前端弹层，回答经
//! WebSocket 回填注册表，再回喂模型。
//!
//! 与 print 模式的 stdin、TUI 的弹层同一契约（[`QuestionSink`]）。在途提问
//! 生命周期（登记 / 应答回填 / 取消丢弃 / 当前快照）收在共享
//! [`QuestionRegistry`]（与 TUI 同一实现，取消语义唯一口径）；本 adapter 只
//! 负责 broadcast：`cancel` 触发时丢弃注册表条目（丢弃成功说明尚未被回答，
//! 广播取消事件让前端收起弹层），立即返回错误结束工具。
//!
//! 问题宿按 session 构建：持有本 session 的 id、事件广播与提问注册表（见
//! [`SessionRuntime`][crate::web::SessionRuntime]），避免跨 session 串流。

use std::sync::Arc;

use async_trait::async_trait;
use nomic_core::ToolError;
use nomic_tools::{AskUserAnswer, AskUserQuestion, QuestionRegistry, QuestionSink};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::web::ServerEvent;

/// web 模式的问题宿：问题广播到本 session 的 WebSocket，等待前端回答。
#[derive(Debug)]
pub struct WebQuestionSink {
    pub session_id: String,
    pub events: broadcast::Sender<ServerEvent>,
    pub registry: Arc<QuestionRegistry>,
}

#[async_trait]
impl QuestionSink for WebQuestionSink {
    async fn ask(
        &self,
        question: AskUserQuestion,
        cancel: CancellationToken,
    ) -> Result<AskUserAnswer, ToolError> {
        let (id, answer_rx) = self.registry.register(question.clone());
        let _ = self.events.send(ServerEvent::Question {
            session_id: self.session_id.clone(),
            id: id.clone(),
            question,
        });

        tokio::select! {
            () = cancel.cancelled() => {
                // 运行被中断：丢弃注册表条目；丢弃成功（尚未被回答）时
                // 通知前端收起弹层
                if self.registry.discard(&id) {
                    let _ = self.events.send(ServerEvent::QuestionCancelled {
                        session_id: self.session_id.clone(),
                        id,
                    });
                }
                Err(ToolError::new("question cancelled (run aborted)"))
            }
            answer = answer_rx => {
                // 回答经注册表回填（条目随 answer 移除）；通道关闭
                // 即条目已被丢弃，转为错误结果
                answer.map_err(|_| ToolError::new("question answer channel closed"))
            }
        }
    }
}
