//! 单会话执行引擎（agent loop 的单轮形态）。
//!
//! worker 从待处理队列领取一个会话，串行执行
//! `组装历史 → 请求 LLM（流式）→ 汇总落库`；每个 checkpoint
//! 落盘完成后才推进下一步，会话执行到静止后释放为 Idle。
//! 请求失败时会话挂回待处理（Running → Pending），等待调度器重驱。

use futures_util::StreamExt;
use nomic_core::{
    Event, LlmAdapter, LlmError, LlmEvent, Message, MessageId, SessionId, SessionState, Step,
    Storage, StorageError,
};

use crate::agent::prompt::assemble_system_prompt;

/// agent loop 执行失败。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// 存储层故障。
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// LLM 适配层故障；重试策略由调度层施加（后续切片）。
    #[error(transparent)]
    Llm(#[from] LlmError),
    /// 系统提示组装时上下文文件读取失败。
    #[error("system prompt context unreadable: {0}")]
    PromptIo(#[from] std::io::Error),
    /// 适配器在未注册任何工具时请求工具调用：协议违约。
    #[error("adapter requested tool call while no tools are registered")]
    UnexpectedToolCall,
    /// 失败悬挂保护自身失败：原始执行错误与挂回错误同时给出。
    /// 会话可能滞留运行中，调用方需按存储故障处理。
    #[error("session suspend failed after {original}; session may remain running: {suspend}")]
    SuspendFailed {
        /// 触发挂回的原始执行错误。
        original: Box<AgentError>,
        /// 挂回操作自身的存储错误。
        suspend: StorageError,
    },
}

/// Agent loop 执行引擎：持有存储与 LLM 适配器，串行执行会话。
///
/// 依赖以引用持有（`'a`）：调度器、广播等其它组件共享同一存储。
/// 后续切片的工具注册表、取消与重试策略在本结构体扩展。
#[derive(Debug)]
pub struct Worker<'a, S, L> {
    storage: &'a S,
    llm: &'a L,
}

impl<'a, S: Storage, L: LlmAdapter> Worker<'a, S, L> {
    /// 创建执行引擎。
    pub const fn new(storage: &'a S, llm: &'a L) -> Self {
        Self { storage, llm }
    }

    /// 从待处理队列领取一个会话并执行到静止，返回该会话 id。
    /// 没有待处理工作时返回 `Ok(None)`。
    ///
    /// `emit` 接收执行过程产生的事件（流式增量、消息完成），事件与
    /// 落库步骤一一对应。请求 LLM 失败时会话挂回待处理，错误向上传播。
    pub async fn run_next_pending(
        &self,
        emit: &mut impl FnMut(Event),
    ) -> Result<Option<SessionId>, AgentError> {
        let Some(session_id) = self.storage.claim_next_pending().await? else {
            return Ok(None);
        };
        match self.run_session(session_id, emit).await {
            Ok(()) => Ok(Some(session_id)),
            Err(error) => {
                // 失败悬挂保护：领取的会话回到待处理，调度器可重驱
                let state = SessionState::Running
                    .suspend()
                    .expect("claimed session must be running");
                match self.storage.set_state(session_id, state).await {
                    Ok(()) => Err(error),
                    // 挂回失败时存储已不可信：原始错误与挂回错误一并给出，
                    // 不相互覆盖
                    Err(suspend) => Err(AgentError::SuspendFailed {
                        original: Box::new(error),
                        suspend,
                    }),
                }
            }
        }
    }

    /// 执行一个已领取（Running）的会话到静止。
    async fn run_session(
        &self,
        session_id: SessionId,
        emit: &mut impl FnMut(Event),
    ) -> Result<(), AgentError> {
        let session = self
            .storage
            .get_session(session_id)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("session {session_id}")))?;

        // 历史回放：已落库的消息步骤 + 运行时确定性重建的系统提示，
        // 组成与恢复路径一致的 LLM 请求序列。
        // 单轮对话的落库步骤只会是消息；工具步骤的上下文回放构造
        // 随工具调用回合切片落地
        let steps = self.storage.steps(session_id).await?;
        let mut messages = Vec::with_capacity(steps.len() + 1);
        messages.push(Message::System {
            id: MessageId::new(),
            text: assemble_system_prompt(session.working_dir())?,
        });
        messages.extend(steps.iter().filter_map(|step| match step {
            Step::Message(message) => Some(message.clone()),
            _ => None,
        }));

        let assistant_id = MessageId::new();
        let mut stream = self.llm.respond(session_id, &messages, &[]).await?;
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event {
                LlmEvent::Token(delta) => {
                    text.push_str(&delta);
                    emit(Event::TokenDelta {
                        session_id,
                        message_id: assistant_id,
                        delta,
                    });
                }
                LlmEvent::ToolCallRequested(_) => return Err(AgentError::UnexpectedToolCall),
            }
        }

        // checkpoint 先于释放：助手消息落库后才推进到静止
        let assistant = Message::Assistant {
            id: assistant_id,
            text,
            tool_calls: Vec::new(),
        };
        self.storage
            .append_step(session_id, Step::Message(assistant.clone()))
            .await?;
        emit(Event::MessageCompleted {
            session_id,
            message: assistant,
        });
        self.storage
            .set_state(
                session_id,
                SessionState::Running
                    .quiesce()
                    .expect("claimed session must be running"),
            )
            .await?;
        Ok(())
    }
}
