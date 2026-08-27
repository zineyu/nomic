//! agent actor：`Agent` 的推荐外部使用方式（ADR-0022）。
//!
//! [`Agent::spawn`] 把 agent 本体移入专属 tokio 任务，任务内串行处理
//! 命令邮箱；调用方持有 [`AgentHandle`]（可克隆、可跨任务分发），全部
//! 交互经邮箱完成——`&mut Agent` 时代的「仅非运行状态调用」纪律对
//! handle 调用方不复存在，命令在 actor 内按到达顺序串行执行。
//!
//! - `prompt` / `continue_run` / `compact` 携带本轮取消令牌，经 oneshot 回执
//!   返回结果；
//! - `inject_user_message`、`set_system_prompt` 等变更为 fire-and-forget：
//!   邮箱 FIFO 即顺序保证，紧随其后的 `prompt` 一定跑在变更之后；
//! - 查询（`messages` / `context_tokens` / `model` / `system_prompt` /
//!   `reasoning` / `stats`）
//!   读 agent 本体维护的共享只读状态视图（ADR-0035），不经邮箱——run 类
//!   命令把整轮 loop 包进一条邮箱命令，运行期间邮箱不被消费，查询若走
//!   邮箱会排队到 run 结束（web `get_state` 超时、切不回活跃会话的根因）。
//!   视图为快照隔离：不保证读到仍在邮箱排队的变更，读己之写先经
//!   [`AgentHandle::flush`] 屏障；
//! - 运行中注入源（[`crate::TurnInjection`]）由 builder 组装进 agent 本体，
//!   turn 边界注入不经邮箱（ADR-0014）。

use std::sync::Arc;

use nomic_ai::{ImageContent, Message, Model, Provider, ThinkingLevel};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use crate::agent::state::{SharedStateView, StateView};
use crate::agent::{Agent, AgentError, SessionStats};
use crate::compaction::{Compaction, CompactionError};

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
    /// 运行时整体替换系统提示词（跨 workspace 恢复 session 后按新
    /// workspace 重建）
    SetSystemPrompt(String),
    /// 运行时切换模型（上下文保留）
    SetModel(Model),
    /// 运行时切换 provider（跨 provider 的模型切换，附分层后的 api_key）
    SetProvider {
        provider: Arc<dyn Provider>,
        api_key: Option<String>,
    },
    /// 设置思考级别
    SetReasoning(Option<ThinkingLevel>),
    /// 屏障：回执送达即此前提交的全部命令已应用（读己之写同步用）
    Flush(oneshot::Sender<()>),
}

/// agent actor 句柄：可克隆、可在任意时机调用，命令经邮箱串行执行。
///
/// 全部命令方法在 actor 任务退出后返回 [`ActorError::Gone`]（fire-and-forget
/// 变更为发送失败；回执类方法为 oneshot 被丢弃）。事件流接收端在
/// builder `build()` 时取得，不经 handle。
///
/// 查询方法读共享状态视图（ADR-0035），不等待邮箱：运行中即时返回最后
/// 一次应用的状态；`flush` 屏障保证此前提交的变更全部应用后再查询。
#[derive(Debug, Clone)]
pub struct AgentHandle {
    cmd_tx: mpsc::UnboundedSender<AgentCommand>,
    /// 共享只读状态视图（agent 本体单写者维护）
    view: SharedStateView,
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

    /// 运行时整体替换系统提示词（消息历史、工具与配置保留；下一次请求
    /// 即携带新提示词）。fire-and-forget：紧随其后的 `prompt` 一定看到
    /// 新提示词（邮箱 FIFO）。
    pub fn set_system_prompt(&self, prompt: String) -> Result<(), ActorError> {
        self.send(AgentCommand::SetSystemPrompt(prompt))
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

    /// 查询当前消息历史（读共享状态视图；快照隔离，不阻塞在途运行）。
    pub fn messages(&self) -> Result<Vec<Message>, ActorError> {
        self.view_read(|view| view.messages.clone())
    }

    /// 查询当前上下文 token 估算（与自动压缩同一口径）。
    pub fn context_tokens(&self) -> Result<u64, ActorError> {
        self.view_read(|view| view.context_tokens)
    }

    /// 查询当前模型。
    pub fn model(&self) -> Result<Model, ActorError> {
        self.view_read(|view| view.model.clone())
    }

    /// 查询当前系统提示词。
    pub fn system_prompt(&self) -> Result<String, ActorError> {
        self.view_read(|view| view.system_prompt.clone())
    }

    /// 查询当前思考级别。
    pub fn reasoning(&self) -> Result<Option<ThinkingLevel>, ActorError> {
        self.view_read(|view| view.reasoning)
    }

    /// 查询当前会话统计信息（前端状态栏展示用）。
    pub fn stats(&self) -> Result<SessionStats, ActorError> {
        self.view_read(|view| view.stats.clone())
    }

    /// 屏障：等待此前提交的全部命令应用完成（邮箱 FIFO）。
    ///
    /// 查询读共享视图、不等邮箱——fire-and-forget 变更（`set_model` 等）
    /// 提交后需要「读到自己的写入」时，先经本屏障同步再查询。
    pub async fn flush(&self) -> Result<(), ActorError> {
        self.call(AgentCommand::Flush).await
    }

    /// 读共享状态视图；actor 已退出（panic）时报告 [`ActorError::Gone`]。
    fn view_read<T>(&self, read: impl FnOnce(&StateView) -> T) -> Result<T, ActorError> {
        if self.cmd_tx.is_closed() {
            return Err(ActorError::Gone);
        }
        Ok(read(&self.view.read().expect("state view lock")))
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
        self.spawn_with_span(None)
    }

    /// 启动 agent actor，并把所有内部日志挂到指定 span 下。
    ///
    /// `parent` 为 `None` 时继承调用者当前 span；这保证默认 `spawn` 行为
    /// 不变，而调用方显式传入 session/request span 后 actor 内所有日志
    /// 自动携带对应字段。
    pub fn spawn_with_span(
        self,
        parent: Option<&tracing::Span>,
    ) -> (AgentHandle, tokio::task::JoinHandle<()>) {
        let span = parent.cloned().unwrap_or_else(tracing::Span::current);
        tracing::debug!(parent = %span.is_none(), "spawning agent actor");
        let view = self.state_view.clone();
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<AgentCommand>();
        let task = tokio::spawn(
            async move {
                let mut agent = self;
                while let Some(command) = cmd_rx.recv().await {
                    match command {
                        AgentCommand::Prompt {
                            text,
                            images,
                            cancel,
                            reply,
                        } => {
                            let _ =
                                reply.send(agent.prompt_with_images(&text, &images, cancel).await);
                        }
                        AgentCommand::Continue { cancel, reply } => {
                            let _ = reply.send(agent.continue_run(cancel).await);
                        }
                        AgentCommand::Compact {
                            instructions,
                            cancel,
                            reply,
                        } => {
                            let _ =
                                reply.send(agent.compact(instructions.as_deref(), cancel).await);
                        }
                        AgentCommand::InjectUserMessage(text) => agent.inject_user_message(&text),
                        AgentCommand::ClearMessages => agent.clear_messages(),
                        AgentCommand::RestoreMessages(messages) => agent.restore_messages(messages),
                        AgentCommand::SetSystemPrompt(prompt) => agent.set_system_prompt(prompt),
                        AgentCommand::SetModel(model) => agent.set_model(model),
                        AgentCommand::SetProvider { provider, api_key } => {
                            agent.set_provider(provider, api_key);
                        }
                        AgentCommand::SetReasoning(level) => agent.set_reasoning(level),
                        AgentCommand::Flush(reply) => {
                            let _ = reply.send(());
                        }
                    }
                }
            }
            .instrument(span),
        );
        (AgentHandle { cmd_tx, view }, task)
    }
}
