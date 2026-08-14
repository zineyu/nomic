//! agent loop：借鉴 pi-agent-core，事件驱动 + 错误编码进流。
//!
//! 生命周期事件：agent → turn → message → tool_execution 四层。
//! 保留的关键行为：
//! - `stop_reason == Length` 时批量失败该消息所有工具调用（参数可能被截断）
//! - parallel/sequential 工具执行；任何工具声明 `Sequential` 则整批串行
//! - 工具错误回喂模型而非中断 loop
//!
//! M1 裁剪（事件枚举预留扩展空间）：follow-up 队列、
//! `prepareNextTurn`、`shouldStopAfterTurn`。
//! 统一消息队列（运行中 turn 边界注入，ADR-0014）已实现，见 [`crate::SteeringQueue`]。

mod actor;
mod events;
mod util;

pub use actor::{ActorError, AgentHandle};
pub use events::AgentEvent;
use util::{FinalizedToolCall, user_message};

use std::sync::Arc;

use nomic_ai::{
    AssistantContent, AssistantEvent, AssistantMessage, Context, ImageContent, Message, Model,
    Provider, StopReason, StreamOptions, ThinkingLevel, ToolCall, ToolResultMessage, Usage,
    UserMessage, UserMessageContent, now_millis,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::compaction::{
    CompactRequest, Compaction, CompactionError, CompactionSettings, compact_messages,
    estimate_context_tokens, should_compact,
};
use crate::hooks::{AfterToolCall, AgentHooks, BeforeToolCall, ToolCallDecision};
use crate::steering::{SteeringMessage, SteeringQueue};
use crate::tool::{DynTool, ExecutionMode, ToolResult, ToolUpdate};

/// loop 配置（crate 内部；外部经 [`Agent::builder`] 组装）。
///
/// `pub` 而非 `pub(crate)`：`agent` 模块本身不对外导出，可见性由模块层控制。
pub struct AgentConfig {
    /// 当前模型
    pub model: Model,
    /// provider 实现
    pub provider: Arc<dyn Provider>,
    /// 流式请求选项
    pub stream_options: StreamOptions,
    /// 生命周期 hooks
    pub hooks: Arc<dyn AgentHooks>,
    /// 默认工具执行模式（默认 parallel）
    pub tool_execution: ExecutionMode,
    /// 上下文压缩配置（`enabled` 只控制自动触发，手动 [`Agent::compact`] 不受限）
    pub compaction: CompactionSettings,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// agent loop 的编程错误（provider 运行时错误编码在消息中，不走这里）。
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// provider 违反了流协议（未以 Done/Error 终止）
    #[error("provider stream contract violated: {0}")]
    StreamContract(String),
}

/// agent：持有消息历史、工具与配置，逐 prompt 驱动 loop。
pub struct Agent {
    config: AgentConfig,
    system_prompt: String,
    messages: Vec<Message>,
    tools: Vec<DynTool>,
    event_tx: mpsc::UnboundedSender<AgentEvent>,
    steering: SteeringQueue,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("config", &self.config)
            .field("messages", &self.messages.len())
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// 创建 agent builder（typestate）：`model` / `provider` / `system_prompt`
    /// 为编译期强制必填项，其余创建项带默认值，见 [`crate::AgentBuilder`]。
    pub fn builder() -> crate::builder::AgentBuilder {
        crate::builder::AgentBuilder::new()
    }

    /// 由 builder 组装完整部件后构造 agent（实现内核，外部不可直接调用）。
    pub(crate) fn from_parts(
        config: AgentConfig,
        tools: Vec<DynTool>,
        system_prompt: String,
        messages: Vec<Message>,
        steering: SteeringQueue,
    ) -> (Self, mpsc::UnboundedReceiver<AgentEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        (
            Self {
                config,
                system_prompt,
                messages,
                tools,
                event_tx,
                steering,
            },
            event_rx,
        )
    }

    /// 统一消息队列句柄（ADR-0014）。
    ///
    /// 交互端持克隆随时入队/编辑（运行期间 driver 串行 job 通道被
    /// prompt 占用，无法中转）；agent 在每个 turn 边界（当前 assistant
    /// turn 的工具调用执行完后、下一次 LLM 调用前）弹出一条注入当前
    /// run（one-at-a-time），队列未清空时 run 不结束。
    pub fn steering_handle(&self) -> SteeringQueue {
        self.steering.clone()
    }

    /// 当前消息历史。
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// 清空消息历史（交互端「开启新对话」语义，如 TUI 的 `/new`）。
    ///
    /// 系统提示词、工具与配置保留；应在非运行状态（`prompt` 返回后）调用。
    pub fn clear_messages(&mut self) {
        self.messages.clear();
    }

    /// 当前模型。
    pub const fn model(&self) -> &Model {
        &self.config.model
    }

    /// 运行时切换模型（交互端 `/models` 语义）。
    ///
    /// 消息历史、系统提示词与工具保留；provider 与 stream options 不变——
    /// 同一 provider 下的模型共享连接参数（base_url 按 Model 逐请求取，
    /// api_key 请求时经 stream options / provider / 环境变量解析）。
    /// 应在非运行状态（`prompt` 返回后）调用。静默切换，不发出事件。
    pub fn set_model(&mut self, model: Model) {
        self.config.model = model;
    }

    /// 运行时切换 provider（跨 provider 的模型切换语义，如 TUI 跨 provider 的
    /// `/models`）。
    ///
    /// 与 [`Self::set_model`] 配对使用：模型跨 provider 时连接实现与
    /// stream options 的 api_key 一并替换（api_key 分层在调用方完成）。
    /// 消息历史、系统提示词与工具保留。应在非运行状态（`prompt` 返回后）
    /// 调用。静默切换，不发出事件。
    pub fn set_provider(&mut self, provider: Arc<dyn Provider>, api_key: Option<String>) {
        self.config.provider = provider;
        self.config.stream_options.api_key = api_key;
    }

    /// 当前思考级别（`StreamOptions::reasoning`）。
    pub const fn reasoning(&self) -> Option<ThinkingLevel> {
        self.config.stream_options.reasoning
    }

    /// 运行时设置思考级别（交互端 `/models` 级别选择器语义）。
    ///
    /// 思考级别是请求参数：仅 `model.reasoning == true` 时随请求生效
    /// （见 [`nomic_ai::StreamOptions::reasoning`]）；消息历史、系统提示词
    /// 与工具保留。应在非运行状态（`prompt` 返回后）调用。静默切换，不发出事件。
    pub const fn set_reasoning(&mut self, reasoning: Option<ThinkingLevel>) {
        self.config.stream_options.reasoning = reasoning;
    }

    /// 以既有消息历史整体替换当前上下文（session resume 语义，如 TUI 的 `/resume`）。
    ///
    /// 与 builder 的 `messages` 同样的调用契约：`messages` 按序作为上下文起点，
    /// 调用方负责保证顺序与来源（如 session store 的 `load_messages` 输出）。
    /// 静默替换，不发出事件（历史已在来源 session 渲染/落库）；
    /// 应在非运行状态（`prompt` 返回后）调用。
    pub fn restore_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    /// 在两轮 prompt 之间向历史注入一条 user 消息（手动载入 skill、外部指令等）。
    ///
    /// 与 [`Self::clear_messages`] 同样的调用契约：仅在非运行状态
    /// （`prompt` 返回后）调用。会发出 `MessageStart`/`MessageEnd` 事件，
    /// 交互端渲染与 session 落库经既有事件管线自动生效。
    pub fn inject_user_message(&mut self, text: &str) {
        let user = Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_string()),
            timestamp: now_millis(),
        });
        self.emit(AgentEvent::MessageStart(Box::new(user.clone())));
        self.messages.push(user.clone());
        self.emit(AgentEvent::MessageEnd(Box::new(user)));
    }

    /// 手动压缩上下文（`/compact [instructions]` 语义）。
    ///
    /// 返回 `Ok(None)` 表示无可压缩内容；失败返回 `Err` 且历史不变。
    /// 应在非运行状态（`prompt` 返回后）调用；取消经 `cancel` 表达。
    pub async fn compact(
        &mut self,
        instructions: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<Option<Compaction>, CompactionError> {
        self.compact_internal(instructions, cancel).await
    }

    /// 压缩的实现内核：生成摘要、替换历史、发出事件。
    async fn compact_internal(
        &mut self,
        instructions: Option<&str>,
        cancel: CancellationToken,
    ) -> Result<Option<Compaction>, CompactionError> {
        let event_tx = self.event_tx.clone();
        let outcome = compact_messages(
            &self.config.provider,
            &self.config.model,
            &self.config.stream_options,
            &self.config.compaction,
            &CompactRequest {
                messages: &self.messages,
                custom_instructions: instructions,
            },
            cancel,
            move |tokens_before| {
                let _ = event_tx.send(AgentEvent::CompactionStart { tokens_before });
            },
        )
        .await?;
        let Some((compaction, new_history)) = outcome else {
            return Ok(None);
        };
        tracing::info!(
            tokens_before = compaction.tokens_before,
            kept_count = compaction.kept_count,
            summarized = self.messages.len() - compaction.kept_count,
            "context compacted"
        );
        self.messages = new_history;
        self.emit(AgentEvent::CompactionEnd {
            summary: compaction.summary.clone(),
            tokens_before: compaction.tokens_before,
            kept_count: compaction.kept_count,
            usage: compaction.usage,
        });
        Ok(Some(compaction))
    }

    /// 发送一个纯文本用户 prompt 并运行 loop 直到完成，返回本次新增的消息。
    ///
    /// 携带图片附件时用 [`Self::prompt_with_images`]。
    /// provider 错误不会让这里返回 `Err`（编码在 assistant 消息中）；
    /// `Err` 仅表示 provider 违反流协议。
    pub async fn prompt(
        &mut self,
        text: &str,
        cancel: CancellationToken,
    ) -> Result<Vec<Message>, AgentError> {
        self.prompt_with_images(text, &[], cancel).await
    }

    /// 发送携带图片附件的用户 prompt，运行 loop 直到完成。
    ///
    /// 有附件时 user 消息为内容块列表：图片块在前、文本块在后（与 Anthropic
    /// 官方建议的排序一致），随历史持久化与回放；压缩对图片块按固定成本估算。
    /// 空附件等价于 [`Self::prompt`]。
    #[tracing::instrument(name = "agent_prompt", skip_all)]
    pub async fn prompt_with_images(
        &mut self,
        text: &str,
        images: &[ImageContent],
        cancel: CancellationToken,
    ) -> Result<Vec<Message>, AgentError> {
        let mut new_messages = Vec::new();
        let user = user_message(text, images);
        tracing::debug!(
            prompt_len = text.len(),
            images = images.len(),
            "agent run started"
        );
        self.emit(AgentEvent::AgentStart);
        self.emit(AgentEvent::MessageStart(Box::new(user.clone())));
        self.messages.push(user.clone());
        new_messages.push(user.clone());
        self.emit(AgentEvent::MessageEnd(Box::new(user)));

        if let Err(error) = self.run_loop(&mut new_messages, cancel).await {
            tracing::error!(%error, "agent run failed");
            return Err(error);
        }

        self.emit(AgentEvent::AgentEnd {
            messages: new_messages.clone(),
        });
        tracing::info!(new_messages = new_messages.len(), "agent run finished");
        Ok(new_messages)
    }

    /// 重试最近一轮失败的响应（`/retry`）：弹出历史尾部失败（Error/Aborted）
    /// 的 assistant 消息，以暴露出的 user 消息为起点重跑 loop。
    ///
    /// 不重新注入 user 消息（原消息含图片附件仍在历史中，不会在上下文里
    /// 重复）；弹出的失败消息不回填——失败/中止的响应不应作为上下文继续
    /// 发给模型（部分内容 + 缺失 toolResult 会被 API 拒绝）。session 落库是
    /// append-only，失败消息仍留在 session 记录中，resume 时按历史回放。
    ///
    /// 无可重试状态（历史不以 user 消息结尾，如最近一轮已成功）返回
    /// `Ok(None)`；与 [`Self::prompt`] 同一错误口径，`Err` 仅表示 provider
    /// 违反流协议。
    pub async fn retry(
        &mut self,
        cancel: CancellationToken,
    ) -> Result<Option<Vec<Message>>, AgentError> {
        while matches!(
            self.messages.last(),
            Some(Message::Assistant(assistant))
                if matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted)
        ) {
            self.messages.pop();
        }
        if !matches!(self.messages.last(), Some(Message::User(_))) {
            return Ok(None);
        }

        let mut new_messages = Vec::new();
        tracing::debug!("agent retry started");
        self.emit(AgentEvent::AgentStart);
        if let Err(error) = self.run_loop(&mut new_messages, cancel).await {
            tracing::error!(%error, "agent retry failed");
            return Err(error);
        }
        self.emit(AgentEvent::AgentEnd {
            messages: new_messages.clone(),
        });
        tracing::info!(new_messages = new_messages.len(), "agent retry finished");
        Ok(Some(new_messages))
    }

    async fn run_loop(
        &mut self,
        new_messages: &mut Vec<Message>,
        cancel: CancellationToken,
    ) -> Result<(), AgentError> {
        loop {
            // 每个 turn 前检查上下文是否逼近窗口（turn 之间压缩，与 pi 一致）；
            // 压缩失败仅告警，保留原历史继续（fail-safe）
            if should_compact(
                estimate_context_tokens(&self.messages),
                self.config.model.context_window,
                &self.config.compaction,
            ) && let Err(error) = self.compact_internal(None, cancel.clone()).await
            {
                tracing::warn!(%error, "auto-compaction failed; continuing with full history");
            }
            self.emit(AgentEvent::TurnStart);
            let message = self.stream_assistant(&cancel).await?;
            let stop_reason = message.stop_reason;
            self.messages.push(Message::Assistant(message.clone()));
            new_messages.push(Message::Assistant(message.clone()));
            self.emit(AgentEvent::MessageEnd(Box::new(Message::Assistant(
                message.clone(),
            ))));

            if matches!(stop_reason, StopReason::Error | StopReason::Aborted) {
                tracing::warn!(
                    stop_reason = ?stop_reason,
                    error = message.error_message.as_deref().unwrap_or(""),
                    "turn ended abnormally"
                );
                self.emit(AgentEvent::TurnEnd {
                    message: Box::new(message),
                    tool_results: Vec::new(),
                });
                return Ok(());
            }

            let tool_calls: Vec<ToolCall> = message
                .content
                .iter()
                .filter_map(|block| match block {
                    AssistantContent::ToolCall(call) => Some(call.clone()),
                    _ => None,
                })
                .collect();
            tracing::debug!(
                stop_reason = ?stop_reason,
                tool_calls = tool_calls.len(),
                input_tokens = message.usage.input,
                output_tokens = message.usage.output,
                cache_read_tokens = message.usage.cache_read,
                "turn completed"
            );

            let mut tool_results = Vec::new();
            let mut terminate = false;
            if !tool_calls.is_empty() {
                let finalized = if stop_reason == StopReason::Length {
                    // 输出被 token 上限截断：所有工具调用的参数都可能不完整，
                    // 执行不安全，批量失败让模型重新发起（与 pi 一致）
                    tracing::warn!(
                        tool_calls = tool_calls.len(),
                        "response hit output token limit; failing all tool calls"
                    );
                    tool_calls
                        .iter()
                        .map(|call| FinalizedToolCall {
                            tool_call: call.clone(),
                            result: ToolResult::text(format!(
                                "Tool call \"{}\" was not executed: the response hit the output token limit, \
                                 so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                                call.name
                            )),
                            is_error: true,
                        })
                        .collect()
                } else {
                    self.execute_tool_calls(&message, &tool_calls, &cancel)
                        .await
                };

                terminate = !finalized.is_empty() && finalized.iter().all(|f| f.result.terminate);
                tool_results = self.record_tool_results(new_messages, finalized);
            }

            self.emit(AgentEvent::TurnEnd {
                message: Box::new(message),
                tool_results,
            });

            if terminate || cancel.is_cancelled() {
                return Ok(());
            }
            // 统一队列注入（pi 式 one-at-a-time，ADR-0014）：turn 边界弹出一条
            // 排队消息注入当前 run（QUEUE 编辑冻结期跳过）；队列未清空时
            // run 不结束——模型无工具调用也注入续行，直至队列排空
            if let Some(steered) = self.steering.pop_front() {
                self.inject_steered(&steered, new_messages);
                continue;
            }
            // 只要执行过工具调用就继续下一 turn（与 pi 一致：不依赖 stop_reason）
            if tool_calls.is_empty() {
                return Ok(());
            }
        }
    }

    /// 注入一条 steering 消息：作为 user 消息进入历史与本次新增，发出
    /// `MessageStart`/`MessageEnd` 事件（交互端渲染与 session 落库经
    /// 既有事件管线自动生效，与 [`Self::inject_user_message`] 同一口径）。
    fn inject_steered(&mut self, steered: &SteeringMessage, new_messages: &mut Vec<Message>) {
        let user = user_message(&steered.text, &steered.images);
        tracing::debug!(text_len = steered.text.len(), "steering message injected");
        self.emit(AgentEvent::MessageStart(Box::new(user.clone())));
        self.messages.push(user.clone());
        new_messages.push(user.clone());
        self.emit(AgentEvent::MessageEnd(Box::new(user)));
    }

    /// 将一批已决工具调用落为 toolResult 消息（历史 + 本次新增），
    /// 并发出 `ToolExecutionEnd` / `MessageStart` / `MessageEnd` 事件。
    fn record_tool_results(
        &mut self,
        new_messages: &mut Vec<Message>,
        finalized: Vec<FinalizedToolCall>,
    ) -> Vec<ToolResultMessage> {
        let mut tool_results = Vec::new();
        for f in finalized {
            self.emit(AgentEvent::ToolExecutionEnd {
                tool_call_id: f.tool_call.id.clone(),
                tool_name: f.tool_call.name.clone(),
                result: f.result.clone(),
                is_error: f.is_error,
            });
            let result_message = ToolResultMessage {
                tool_call_id: f.tool_call.id,
                tool_name: f.tool_call.name,
                content: f.result.content,
                details: f.result.details,
                is_error: f.is_error,
                timestamp: now_millis(),
            };
            self.emit(AgentEvent::MessageStart(Box::new(Message::ToolResult(
                result_message.clone(),
            ))));
            self.messages
                .push(Message::ToolResult(result_message.clone()));
            new_messages.push(Message::ToolResult(result_message.clone()));
            tool_results.push(result_message);
        }
        for result in &tool_results {
            self.emit(AgentEvent::MessageEnd(Box::new(Message::ToolResult(
                result.clone(),
            ))));
        }
        tool_results
    }

    /// 流式获取一次 assistant 响应。
    async fn stream_assistant(
        &self,
        cancel: &CancellationToken,
    ) -> Result<AssistantMessage, AgentError> {
        let context = Context {
            system_prompt: Some(self.system_prompt.clone()),
            messages: self.messages.clone(),
            tools: self.tools.iter().map(DynTool::definition).collect(),
        };
        let stream = self.config.provider.stream(
            &self.config.model,
            &context,
            &self.config.stream_options,
            cancel.clone(),
        );

        // provider 在流建立前失败（重试耗尽、连接失败等）时不发 Start，
        // 只发 Error 终止事件；记录 Start 是否到达，必要时在终止后补发
        // MessageStart，保证 MessageStart/MessageEnd 始终配对
        let mut started = false;
        let message = stream
            .result_with(|event| match event {
                AssistantEvent::Start => {
                    started = true;
                    let skeleton = AssistantMessage {
                        content: Vec::new(),
                        api: self.config.model.api,
                        provider: self.config.model.provider.clone(),
                        model: self.config.model.id.clone(),
                        response_model: None,
                        response_id: None,
                        usage: Usage::default(),
                        stop_reason: StopReason::Stop,
                        error_message: None,
                        timestamp: now_millis(),
                    };
                    self.emit(AgentEvent::MessageStart(Box::new(Message::Assistant(
                        skeleton,
                    ))));
                }
                delta => {
                    self.emit(AgentEvent::MessageUpdate(delta));
                }
            })
            .await
            .map_err(|err| AgentError::StreamContract(err.to_string()))?;
        if !started {
            self.emit(AgentEvent::MessageStart(Box::new(Message::Assistant(
                message.clone(),
            ))));
        }
        Ok(message)
    }

    /// 执行一批工具调用（按配置与工具声明选择 parallel / sequential）。
    async fn execute_tool_calls(
        &self,
        message: &AssistantMessage,
        tool_calls: &[ToolCall],
        cancel: &CancellationToken,
    ) -> Vec<FinalizedToolCall> {
        let sequential = self.config.tool_execution == ExecutionMode::Sequential
            || tool_calls.iter().any(|call| {
                self.tools.iter().any(|t| {
                    t.name() == call.name && t.execution_mode() == ExecutionMode::Sequential
                })
            });

        // 预备阶段（串行）：查找工具 + hooks 门控；失败转为即时错误结果
        let mut prepared: Vec<Result<(&ToolCall, DynTool), FinalizedToolCall>> = Vec::new();
        for call in tool_calls {
            self.emit(AgentEvent::ToolExecutionStart {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                args: call.arguments.clone(),
            });
            prepared.push(self.prepare_tool_call(message, call).await);
        }

        if sequential {
            let mut finalized = Vec::new();
            for entry in prepared {
                let f = match entry {
                    Err(immediate) => immediate,
                    Ok((call, tool)) => {
                        self.execute_and_finalize(message, call, tool, cancel).await
                    }
                };
                finalized.push(f);
                if cancel.is_cancelled() {
                    break;
                }
            }
            return finalized;
        }

        let futures: Vec<_> = prepared
            .into_iter()
            .map(|entry| async move {
                match entry {
                    Err(immediate) => immediate,
                    Ok((call, tool)) => {
                        self.execute_and_finalize(message, call, tool, cancel).await
                    }
                }
            })
            .collect();
        futures::future::join_all(futures).await
    }

    /// 预备一个工具调用：找到工具、过 `before_tool_call` 门控。
    #[allow(clippy::result_large_err)] // FinalizedToolCall 体积小，直接内联错误路径
    async fn prepare_tool_call<'a>(
        &self,
        message: &AssistantMessage,
        call: &'a ToolCall,
    ) -> Result<(&'a ToolCall, DynTool), FinalizedToolCall> {
        let Some(tool) = self.tools.iter().find(|t| t.name() == call.name).cloned() else {
            tracing::warn!(tool = %call.name, "tool not found");
            return Err(FinalizedToolCall {
                tool_call: call.clone(),
                result: ToolResult::text(format!("Tool {} not found", call.name)),
                is_error: true,
            });
        };
        let decision = self
            .config
            .hooks
            .before_tool_call(&BeforeToolCall {
                assistant_message: message,
                tool_call: call,
            })
            .await;
        if let ToolCallDecision::Block { reason } = decision {
            tracing::warn!(tool = %call.name, %reason, "tool call blocked by hook");
            return Err(FinalizedToolCall {
                tool_call: call.clone(),
                result: ToolResult::text(reason),
                is_error: true,
            });
        }
        Ok((call, tool))
    }

    /// 执行单个工具调用并过 `after_tool_call` 改写。
    #[tracing::instrument(name = "tool_execution", skip_all, fields(tool = %call.name, id = %call.id))]
    async fn execute_and_finalize(
        &self,
        message: &AssistantMessage,
        call: &ToolCall,
        tool: DynTool,
        cancel: &CancellationToken,
    ) -> FinalizedToolCall {
        tracing::debug!(args = %call.arguments, "tool call args");
        let event_tx = self.event_tx.clone();
        let update_id = call.id.clone();
        let update_name = call.name.clone();
        let on_update = Box::new(move |partial: ToolUpdate| {
            let _ = event_tx.send(AgentEvent::ToolExecutionUpdate {
                tool_call_id: update_id.clone(),
                tool_name: update_name.clone(),
                partial,
            });
        });

        let (mut result, mut is_error) = match tool
            .execute(call.arguments.clone(), cancel.clone(), on_update)
            .await
        {
            Ok(result) => (result, false),
            Err(error) => {
                tracing::warn!("tool execution failed");
                tracing::debug!(error = %error, "tool error detail");
                (ToolResult::text(error.to_string()), true)
            }
        };

        if let Some(over) = self
            .config
            .hooks
            .after_tool_call(&AfterToolCall {
                assistant_message: message,
                tool_call: call,
                result: &result,
                is_error,
            })
            .await
        {
            if let Some(content) = over.content {
                result.content = content;
            }
            if let Some(details) = over.details {
                result.details = Some(details);
            }
            if let Some(flag) = over.is_error {
                is_error = flag;
            }
            if let Some(terminate) = over.terminate {
                result.terminate = terminate;
            }
        }

        FinalizedToolCall {
            tool_call: call.clone(),
            result,
            is_error,
        }
    }

    fn emit(&self, event: AgentEvent) {
        // 无消费者时静默丢弃（print 模式总会消费；嵌入式可自行选择）
        let _ = self.event_tx.send(event);
    }
}
