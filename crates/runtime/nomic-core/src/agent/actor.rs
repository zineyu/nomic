//! agent actor：`Agent` 的推荐外部使用方式（ADR-0022）。
//!
//! [`Agent::spawn`] 把 agent 本体移入专属 tokio 任务，任务内串行处理
//! 命令邮箱；调用方持有 [`AgentHandle`]（可克隆、可跨任务分发），全部
//! 交互经邮箱完成——`&mut Agent` 时代的「仅非运行状态调用」纪律对
//! handle 调用方不复存在，命令在 actor 内按到达顺序串行执行。
//!
//! - `prompt` / `continue_run` / `compact` 携带本轮取消令牌，经 oneshot 回执
//!   返回结果；
//! - `inject_user_message` 等变更为 fire-and-forget：邮箱 FIFO 即顺序
//!   保证，紧随其后的 `prompt` 一定跑在变更之后；
//! - 查询（`messages` / `context_tokens` / `model` / `reasoning`）同样
//!   走邮箱 oneshot（严格 actor，不引入共享只读快照）；
//! - 运行中注入源（[`crate::TurnInjection`]）由 builder 组装进 agent 本体，
//!   turn 边界注入不经邮箱（ADR-0014）。

use std::sync::Arc;

use nomic_ai::{ImageContent, Message, Model, Provider, ThinkingLevel};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentError, SessionStats};
use crate::compaction::{Compaction, CompactionError, estimate_context_tokens};

/// actor 调用错误：actor 任务已退出，或 loop / 压缩本身失败。
#[derive(Debug, thiserror::Error)]
pub enum ActorError {
    /// actor 任务已退出（panic，或全部句柄断开后自然退出）：
    /// 命令发送失败，或回执 oneshot 随任务退出被丢弃
    #[error("agent actor 已退出")]
    Gone,
    /// agent loop 错误（provider 流协议违反）
    #[error(transparent)]
    Loop(#[from] AgentError),
    /// 上下文压缩失败
    #[error(transparent)]
    Compaction(#[from] CompactionError),
}

/// 提交给 actor 任务的命令（crate 私有；外部经 [`AgentHandle`] 方法构造）。
enum AgentCommand {
    /// 运行一轮 prompt（附图片附件与本轮取消令牌）
    Prompt {
        text: String,
        images: Vec<ImageContent>,
        cancel: CancellationToken,
        reply: oneshot::Sender<Result<Vec<Message>, AgentError>>,
    },
    /// 续跑：重发历史尾部的消息（user 消息或 tool result）
    Continue {
        cancel: CancellationToken,
        reply: oneshot::Sender<Result<Option<Vec<Message>>, AgentError>>,
    },
    /// 手动压缩上下文
    Compact {
        instructions: Option<String>,
        cancel: CancellationToken,
        reply: oneshot::Sender<Result<Option<Compaction>, CompactionError>>,
    },
    /// 向历史注入一条 user 消息（手动载入 skill、外部指令等）
    InjectUserMessage(String),
    /// 清空消息历史（新对话语义）
    ClearMessages,
    /// 以既有消息历史整体替换当前上下文（session resume 语义）
    RestoreMessages(Vec<Message>),
    /// 运行时切换模型（上下文保留）
    SetModel(Model),
    /// 运行时切换 provider（跨 provider 的模型切换，附分层后的 api_key）
    SetProvider {
        provider: Arc<dyn Provider>,
        api_key: Option<String>,
    },
    /// 设置思考级别
    SetReasoning(Option<ThinkingLevel>),
    /// 查询当前消息历史
    Messages(oneshot::Sender<Vec<Message>>),
    /// 查询当前上下文 token 估算（与自动压缩同一口径）
    ContextTokens(oneshot::Sender<u64>),
    /// 查询当前模型
    Model(oneshot::Sender<Model>),
    /// 查询当前思考级别
    Reasoning(oneshot::Sender<Option<ThinkingLevel>>),
    /// 查询当前会话统计信息
    Stats(oneshot::Sender<SessionStats>),
}

/// agent actor 句柄：可克隆、可在任意时机调用，命令经邮箱串行执行。
///
/// 全部方法在 actor 任务退出后返回 [`ActorError::Gone`]（fire-and-forget
/// 变更为发送失败；回执类方法为 oneshot 被丢弃）。事件流接收端在
/// builder `build()` 时取得，不经 handle。
#[derive(Debug, Clone)]
pub struct AgentHandle {
    cmd_tx: mpsc::UnboundedSender<AgentCommand>,
}

impl AgentHandle {
    /// 发送一个纯文本用户 prompt 并运行 loop 直到完成，返回本次新增的消息。
    ///
    /// 携带图片附件时用 [`Self::prompt_with_images`]。语义同
    /// [`Agent::prompt`]，错误经 [`ActorError::Loop`] 透传。
    pub async fn prompt(
        &self,
        text: &str,
        cancel: CancellationToken,
    ) -> Result<Vec<Message>, ActorError> {
        self.prompt_with_images(text, &[], cancel).await
    }

    /// 发送携带图片附件的用户 prompt，运行 loop 直到完成。
    pub async fn prompt_with_images(
        &self,
        text: &str,
        images: &[ImageContent],
        cancel: CancellationToken,
    ) -> Result<Vec<Message>, ActorError> {
        Ok(self
            .call(|reply| AgentCommand::Prompt {
                text: text.to_string(),
                images: images.to_vec(),
                cancel,
                reply,
            })
            .await??)
    }

    /// 续跑：重发历史尾部的消息（user 消息或 tool result）；尾部不是
    /// 可续跑消息时返回 `Ok(None)`。语义同 [`Agent::continue_run`]。
    pub async fn continue_run(
        &self,
        cancel: CancellationToken,
    ) -> Result<Option<Vec<Message>>, ActorError> {
        Ok(self
            .call(|reply| AgentCommand::Continue { cancel, reply })
            .await??)
    }

    /// 手动压缩上下文（`/compact [聚焦指令]` 语义）；无可压缩内容返回
    /// `Ok(None)`。语义同 [`Agent::compact`]，错误经
    /// [`ActorError::Compaction`] 透传。
    pub async fn compact(
        &self,
        instructions: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<Option<Compaction>, ActorError> {
        Ok(self
            .call(|reply| AgentCommand::Compact {
                instructions: instructions.map(str::to_string),
                cancel,
                reply,
            })
            .await??)
    }

    /// 在两轮 prompt 之间向历史注入一条 user 消息。fire-and-forget：
    /// 紧随其后的 `prompt` 一定看到该消息（邮箱 FIFO）。
    pub fn inject_user_message(&self, text: &str) -> Result<(), ActorError> {
        self.send(AgentCommand::InjectUserMessage(text.to_string()))
    }

    /// 清空消息历史（新对话语义；系统提示词、工具与配置保留）。
    pub fn clear_messages(&self) -> Result<(), ActorError> {
        self.send(AgentCommand::ClearMessages)
    }

    /// 以既有消息历史整体替换当前上下文（session resume 语义）。
    pub fn restore_messages(&self, messages: Vec<Message>) -> Result<(), ActorError> {
        self.send(AgentCommand::RestoreMessages(messages))
    }

    /// 运行时切换模型（消息历史、系统提示词与工具保留）。
    pub fn set_model(&self, model: Model) -> Result<(), ActorError> {
        self.send(AgentCommand::SetModel(model))
    }

    /// 运行时切换 provider（与 [`Self::set_model`] 配对用于跨 provider
    /// 切换；api_key 分层在调用方完成）。
    pub fn set_provider(
        &self,
        provider: Arc<dyn Provider>,
        api_key: Option<String>,
    ) -> Result<(), ActorError> {
        self.send(AgentCommand::SetProvider { provider, api_key })
    }

    /// 设置思考级别（仅 `model.reasoning == true` 时随请求生效）。
    pub fn set_reasoning(&self, reasoning: Option<ThinkingLevel>) -> Result<(), ActorError> {
        self.send(AgentCommand::SetReasoning(reasoning))
    }

    /// 查询当前消息历史。
    pub async fn messages(&self) -> Result<Vec<Message>, ActorError> {
        self.call(AgentCommand::Messages).await
    }

    /// 查询当前上下文 token 估算（与自动压缩同一口径）。
    pub async fn context_tokens(&self) -> Result<u64, ActorError> {
        self.call(AgentCommand::ContextTokens).await
    }

    /// 查询当前模型。
    pub async fn model(&self) -> Result<Model, ActorError> {
        self.call(AgentCommand::Model).await
    }

    /// 查询当前思考级别。
    pub async fn reasoning(&self) -> Result<Option<ThinkingLevel>, ActorError> {
        self.call(AgentCommand::Reasoning).await
    }

    /// 查询当前会话统计信息（前端状态栏展示用）。
    pub async fn stats(&self) -> Result<SessionStats, ActorError> {
        self.call(AgentCommand::Stats).await
    }

    /// 发送一条 fire-and-forget 命令；邮箱关闭（actor 已退出）时报错。
    fn send(&self, command: AgentCommand) -> Result<(), ActorError> {
        self.cmd_tx.send(command).map_err(|_| ActorError::Gone)
    }

    /// 发送一条携带回执的命令并等待结果。
    async fn call<T>(
        &self,
        command: impl FnOnce(oneshot::Sender<T>) -> AgentCommand,
    ) -> Result<T, ActorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send(command(reply_tx))?;
        reply_rx.await.map_err(|_| ActorError::Gone)
    }
}

impl Agent {
    /// 启动 agent actor：本体移入专属 tokio 任务，串行处理命令邮箱。
    ///
    /// 返回句柄与任务的 `JoinHandle`：任务在全部句柄断开（邮箱关闭）
    /// 后退出；任务 panic 时经 `JoinHandle` 暴露详情，此后全部 handle
    /// 调用返回 [`ActorError::Gone`]，事件通道随 agent 丢弃而关闭。
    /// 事件流接收端在 builder `build()` 时取得，与 spawn 无关。
    pub fn spawn(self) -> (AgentHandle, tokio::task::JoinHandle<()>) {
        tracing::debug!("spawning agent actor");
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AgentCommand>();
        let task = tokio::spawn(async move {
            let mut agent = self;
            while let Some(command) = cmd_rx.recv().await {
                match command {
                    AgentCommand::Prompt {
                        text,
                        images,
                        cancel,
                        reply,
                    } => {
                        let _ = reply.send(agent.prompt_with_images(&text, &images, cancel).await);
                    }
                    AgentCommand::Continue { cancel, reply } => {
                        let _ = reply.send(agent.continue_run(cancel).await);
                    }
                    AgentCommand::Compact {
                        instructions,
                        cancel,
                        reply,
                    } => {
                        let _ = reply.send(agent.compact(instructions.as_deref(), cancel).await);
                    }
                    AgentCommand::InjectUserMessage(text) => agent.inject_user_message(&text),
                    AgentCommand::ClearMessages => agent.clear_messages(),
                    AgentCommand::RestoreMessages(messages) => agent.restore_messages(messages),
                    AgentCommand::SetModel(model) => agent.set_model(model),
                    AgentCommand::SetProvider { provider, api_key } => {
                        agent.set_provider(provider, api_key);
                    }
                    AgentCommand::SetReasoning(level) => agent.set_reasoning(level),
                    AgentCommand::Messages(reply) => {
                        let _ = reply.send(agent.messages().to_vec());
                    }
                    AgentCommand::ContextTokens(reply) => {
                        let _ = reply.send(estimate_context_tokens(agent.messages()));
                    }
                    AgentCommand::Model(reply) => {
                        let _ = reply.send(agent.model().clone());
                    }
                    AgentCommand::Reasoning(reply) => {
                        let _ = reply.send(agent.reasoning());
                    }
                    AgentCommand::Stats(reply) => {
                        let _ = reply.send(agent.stats().clone());
                    }
                }
            }
        });
        (AgentHandle { cmd_tx }, task)
    }
}
