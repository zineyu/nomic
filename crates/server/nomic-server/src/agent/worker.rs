//! 单会话执行引擎（agent loop）。
//!
//! worker 从待处理队列领取一个会话，串行执行
//! `组装历史 → 请求 LLM（流式）→ 工具调用 → 执行环境派发 →
//! 结果回灌 → …`，每个 checkpoint 落盘完成后才推进下一步，
//! 会话执行到静止后释放为 Idle。请求失败时会话挂回待处理
//! （Running → Pending），等待调度器重驱。

use futures_util::StreamExt;
use nomic_core::{
    EnvironmentError, Event, ExecutionEnvironment, LlmAdapter, LlmError, LlmEvent, Message,
    MessageId, SessionId, SessionState, Step, Storage, StorageError, ToolCall,
};

use crate::agent::history::replay_messages;
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
    /// 执行环境派发故障；工具语义层面的成功/报错/拒绝经
    /// `ToolResult` 三层回灌模型，不在本变体内。
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    /// 系统提示组装时上下文文件读取失败。
    #[error("system prompt context unreadable: {0}")]
    PromptIo(#[from] std::io::Error),
    /// 落库历史违反步骤配对不变式（如工具调用开始缺少对应结束）。
    #[error("broken step history: {0}")]
    BrokenHistory(String),
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

/// 执行失败：携带出错会话坐标。
///
/// 调度与事件广播需要会话 id 定位（如失败时向订阅者发出
/// 错误事件），而 [`AgentError`] 本身不携带会话信息。
#[derive(Debug, thiserror::Error)]
#[error("session {session}: {error}")]
pub struct AgentRunError {
    /// 出错的会话。
    pub session: SessionId,
    /// 失败类别。
    #[source]
    pub error: AgentError,
}

/// 领取或执行失败。
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// 待处理队列读取失败：此时没有会话被领取，无会话坐标。
    #[error(transparent)]
    Queue(#[from] StorageError),
    /// 会话执行失败：会话已挂回待处理，坐标在场。
    #[error(transparent)]
    Session(#[from] AgentRunError),
}

/// Agent loop 执行引擎：持有存储、LLM 适配器与执行环境，串行执行会话。
///
/// 依赖以引用持有（`'a`）：调度器、广播等其它组件共享同一存储。
/// 后续切片的取消与重试策略在本结构体扩展。
#[derive(Debug)]
pub struct Worker<'a, S, L, E> {
    storage: &'a S,
    llm: &'a L,
    env: &'a E,
}

impl<'a, S: Storage, L: LlmAdapter, E: ExecutionEnvironment> Worker<'a, S, L, E> {
    /// 创建执行引擎。
    pub const fn new(storage: &'a S, llm: &'a L, env: &'a E) -> Self {
        Self { storage, llm, env }
    }

    /// 领取调度请求指定的会话并执行到静止。
    ///
    /// 会话不在待处理（已静止、运行中或不存在）时返回 `Ok(false)`，
    /// 不产生任何副作用——调度请求幂等。调度单位是显式请求的
    /// 会话 id（受理/恢复），不是全局队列：失败挂回待处理的会话
    /// 不会被无关请求重复领取。
    ///
    /// `emit` 接收执行过程产生的事件（流式增量、消息完成、
    /// 工具调用开始/结束）；每个事件对应其 checkpoint 落库步骤
    /// （流式增量除外）。请求 LLM
    /// 或派发工具失败时会话挂回待处理，错误向上传播。
    pub async fn run_session(
        &self,
        session_id: SessionId,
        emit: &mut impl FnMut(Event),
    ) -> Result<bool, RunError> {
        if !self.storage.claim_session(session_id).await? {
            return Ok(false);
        }
        match self.execute_session(session_id, emit).await {
            Ok(()) => Ok(true),
            Err(error) => {
                // 失败悬挂保护：领取的会话回到待处理，调度器可重驱
                let state = SessionState::Running
                    .suspend()
                    .expect("claimed session must be running");
                let error = match self.storage.set_state(session_id, state).await {
                    Ok(()) => error,
                    // 挂回失败时存储已不可信：原始错误与挂回错误一并给出，
                    // 不相互覆盖
                    Err(suspend) => AgentError::SuspendFailed {
                        original: Box::new(error),
                        suspend,
                    },
                };
                Err(AgentRunError {
                    session: session_id,
                    error,
                }
                .into())
            }
        }
    }

    /// 执行一个已领取（Running）的会话到静止。
    async fn execute_session(
        &self,
        session_id: SessionId,
        emit: &mut impl FnMut(Event),
    ) -> Result<(), AgentError> {
        let session = self
            .storage
            .get_session(session_id)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("session {session_id}")))?;

        // 工具回合循环：每轮请求 LLM，若其请求工具调用则派发执行、
        // 结果回灌后继续下一轮，直到一轮不再请求工具调用。
        loop {
            // 历史回放：已落库步骤经显式构造（工具步骤配对为工具结果
            // 消息）+ 运行时确定性重建的系统提示，与恢复路径一致。
            let steps = self.storage.steps(session_id).await?;
            let mut messages = Vec::with_capacity(steps.len() + 1);
            messages.push(Message::System {
                id: MessageId::new(),
                text: assemble_system_prompt(session.working_dir())?,
            });
            messages.extend(replay_messages(&steps)?);

            let assistant_id = MessageId::new();
            let mut stream = self
                .llm
                .respond(session_id, &messages, self.env.tool_specs())
                .await?;
            let mut text = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
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
                    LlmEvent::ToolCallRequested(call) => calls.push(call),
                }
            }

            // checkpoint 先于推进：助手消息（含本回合工具调用请求）落库
            let assistant = Message::Assistant {
                id: assistant_id,
                text,
                tool_calls: calls.clone(),
            };
            self.storage
                .append_step(session_id, Step::Message(assistant.clone()))
                .await?;
            emit(Event::MessageCompleted {
                session_id,
                message: assistant,
            });

            if calls.is_empty() {
                break;
            }

            for call in calls {
                self.run_tool_call(session_id, session.working_dir(), &call, emit)
                    .await?;
            }
        }

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

    /// 执行一次工具调用并把结果回灌为工具结果消息。
    ///
    /// 逐步 checkpoint：开始步骤落库后才派发，结果步骤落库后才发出
    /// 完成事件并进入下一轮请求。回灌消息不落库——它由历史回放从
    /// `ToolCallStarted` + `ToolCallFinished` 配对唯一构造，
    /// 结果步骤是工具结果的单事实源。
    async fn run_tool_call(
        &self,
        session_id: SessionId,
        working_dir: &std::path::Path,
        call: &ToolCall,
        emit: &mut impl FnMut(Event),
    ) -> Result<(), AgentError> {
        self.storage
            .append_step(session_id, Step::ToolCallStarted { call: call.clone() })
            .await?;
        emit(Event::ToolCallStarted {
            session_id,
            call: call.clone(),
        });

        let result = match self.env.execute(session_id, working_dir, call).await {
            Ok(result) => result,
            Err(error) => {
                // 派发故障：调用状态不确定且无已提交输出，按 interrupted
                // 闭合历史——重驱时回放可构造回灌消息，恢复面保持打开
                self.storage
                    .append_step(
                        session_id,
                        Step::ToolCallInterrupted {
                            call_id: call.id,
                            partial_output: None,
                        },
                    )
                    .await?;
                return Err(error.into());
            }
        };

        self.storage
            .append_step(
                session_id,
                Step::ToolCallFinished {
                    call_id: call.id,
                    result: result.clone(),
                },
            )
            .await?;
        emit(Event::ToolCallFinished {
            session_id,
            call_id: call.id,
            result: result.clone(),
        });
        // 回灌消息由回放从配对步骤构造；此处事件负载为同一构造
        emit(Event::MessageCompleted {
            session_id,
            message: Message::Tool {
                id: MessageId::new(),
                call_id: call.id,
                result,
            },
        });
        Ok(())
    }
}
