//! 会话构建与事件落库：SessionFactory 按 bootstrap 输入构建每个
//! [`SessionRuntime`]（agent actor + session runner + 事件转发任务），
//! 事件转发 / 快照收集收在本模块。run 类 job 的串行消费、取消与
//! 生命周期翻译收在 core 的 [`SessionRunner`]（ADR-0033）；本模块只做
//! runner 事件 → [`ServerEvent`] 的 broadcast 翻译。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use nomic_ai::{Message, Model, Provider, StreamOptions, ThinkingLevel};
use nomic_core::{
    Agent, AgentEvent, CompactOutcome, ContinueOutcome, JobKind, JobOutcome, NOTHING_TO_COMPACT,
    NOTHING_TO_CONTINUE, RunnerEvent, SessionJob, SessionRunner,
};
use nomic_session::{SessionRecorder, SessionStore};
use nomic_skills::SkillResolver;
use nomic_tools::{AskUserQuestion, QuestionRegistry};
use tokio::sync::{Mutex, broadcast, mpsc};

use super::{MessageQueue, QueueEntryView, ServerEvent, SessionRuntime};
use crate::model::ModelResolver;
use crate::web::question::WebQuestionSink;

/// 解析完成的会话模型三元组：模型、provider 连接与流选项（api_key / 推理）。
pub struct ResolvedSessionModel {
    pub model: Model,
    pub provider: Arc<dyn Provider>,
    pub options: StreamOptions,
}

/// 构建新 [`SessionRuntime`] 所需的 bootstrap 输入（进程级共享、不可变）。
pub struct SessionFactory {
    pub models: Arc<ModelResolver>,
    pub system_prompt: String,
    pub skill_resolver: SkillResolver,
    pub stream_options: StreamOptions,
    pub compaction: nomic_core::CompactionSettings,
    pub default_model: Model,
    pub default_reasoning: Option<ThinkingLevel>,
    /// 所有可用模型列表（子 agent 模型选择用）
    pub available_models: Vec<Model>,
    /// 全局事件总线（所有 session 的事件统一发往此处）
    pub events: broadcast::Sender<ServerEvent>,
}

impl SessionFactory {
    /// 解析某 session 的模型 / provider / 流选项：会话级 config 覆盖，否则
    /// 回退进程默认（bootstrap 已按 CLI > 全局 config 解析）。
    pub async fn resolve_session_model(
        &self,
        store: Option<&SessionStore>,
        session_id: &str,
    ) -> ResolvedSessionModel {
        let model = self.session_model(store, session_id).await;
        let reasoning = self.session_reasoning(store, session_id).await;
        let api_key = crate::model::resolve_api_key(
            None,
            std::env::var(crate::model::api_key_env(model.api))
                .ok()
                .as_deref(),
            self.models
                .provider_config(&model.provider)
                .and_then(|p| p.api_key.as_deref()),
            self.models.config().and_then(|c| c.api_key.as_deref()),
        );
        let provider = crate::model::build_provider(model.api, api_key.clone());
        let mut options = self.stream_options.clone();
        options.api_key = api_key;
        options.reasoning = reasoning;
        ResolvedSessionModel {
            model,
            provider,
            options,
        }
    }

    /// 会话级模型 spec：优先读会话级 config，失效或缺失回退进程默认。
    async fn session_model(&self, store: Option<&SessionStore>, session_id: &str) -> Model {
        if let Some(store) = store
            && let Some(spec) = store
                .get_session_config::<String>(session_id, crate::model::CONFIG_KEY_MODEL)
                .await
                .ok()
                .flatten()
            && let Ok(selection) = crate::model::ModelSelection::parse(&spec, None)
            && let Ok(model) = self.models.resolve(&selection.provider, &selection.model)
        {
            return model;
        }
        self.default_model.clone()
    }

    /// 会话级思考级别：优先读会话级 config，缺失回退进程默认。
    async fn session_reasoning(
        &self,
        store: Option<&SessionStore>,
        session_id: &str,
    ) -> Option<ThinkingLevel> {
        if let Some(store) = store
            && let Some(word) = store
                .get_session_config::<String>(session_id, crate::model::CONFIG_KEY_REASONING)
                .await
                .ok()
                .flatten()
        {
            return word.parse().ok();
        }
        self.default_reasoning
    }

    /// 构建并注册一个 [`SessionRuntime`]（含 agent actor 与事件转发任务）。
    ///
    /// `workspace` 是本 session 的操作基准（workspace 严格归属）：工具的
    /// 相对路径以它解析，快照展示同一值。
    pub fn build(
        &self,
        store: Option<SessionStore>,
        id: String,
        history: Vec<Message>,
        tip: Option<String>,
        workspace: PathBuf,
        resolved: ResolvedSessionModel,
    ) -> Arc<SessionRuntime> {
        let events_tx = self.events.clone();
        let recorder = store.map(|store| SessionRecorder::with_tip(store, id.clone(), tip));
        // 在途提问注册表：sink（登记 / 取消丢弃）与 SessionRuntime（应答
        // 回填 / 断线重放快照）共享同一份，取消语义与 TUI 同一口径
        let questions = Arc::new(QuestionRegistry::new());
        let sink = Arc::new(WebQuestionSink {
            session_id: id.clone(),
            events: events_tx.clone(),
            registry: questions.clone(),
        });
        // steering 统一消息队列（ADR-0014/0027，web 侧实现）：与 agent
        // 共享同一份并作为 core 的注入源（turn 边界弹出队首注入本轮）；
        // 运行中提交的 prompt 由 handler 入队，mention 在投递时展开
        let queue = MessageQueue::new(id.clone(), events_tx.clone(), {
            let skills = self.skill_resolver.clone();
            let base = workspace.clone();
            Some(Arc::new(move |text: &str| {
                crate::mention::expand_mentions(text, &skills, &base)
            }))
        });
        // 工具配方（组装收在 agent_recipe 模块）：web 的差异点——主/子
        // agent 各自独立的 todo 清单、提问走事件总线、steering 经统一
        // 消息队列注入；主/子 agent 工具都以本 session 的 workspace 为
        // 基准（严格归属）
        let recipe = crate::agent_recipe::assemble(crate::agent_recipe::RecipeOpts {
            base: nomic_tools::BaseDir::new(Some(workspace.clone())),
            skill_resolver: self.skill_resolver.clone(),
            question_sink: sink,
            todo: crate::agent_recipe::TodoPolicy::Isolated,
            provider: resolved.provider.clone(),
            available_models: self.available_models.clone(),
            turn_injection: Some(Arc::new(queue.clone())),
        });
        let (agent, events_rx) = recipe
            .apply(
                Agent::builder()
                    .model(resolved.model)
                    .provider(resolved.provider)
                    .system_prompt(self.system_prompt.clone()),
            )
            .messages(history)
            .stream_options(resolved.options)
            .compaction(self.compaction)
            .build();
        let (handle, _actor_task) = agent.spawn();
        let (runner, runner_events, _runner_task) = SessionRunner::spawn(handle.clone());

        let session = Arc::new(SessionRuntime {
            id,
            handle,
            recorder: Mutex::new(recorder),
            events: events_tx,
            runner,
            questions,
            queue,
            workspace,
        });
        tokio::spawn(forward_events(session.clone(), events_rx));
        tokio::spawn(forward_runner_events(session.clone(), runner_events));
        session
    }
}

/// 事件转发任务：消费 agent 事件流，先经 [`SessionRecorder`] 落库（定稿点，
/// 失败仅告警，与 print/TUI 同一口径），再发往全局事件总线（携带 `session_id`）。
async fn forward_events(
    session: Arc<SessionRuntime>,
    mut events: mpsc::UnboundedReceiver<AgentEvent>,
) {
    while let Some(event) = events.recv().await {
        let mut recorder = session.recorder.lock().await;
        if let Some(recorder) = &mut *recorder
            && let Err(error) = recorder.record(&event).await
        {
            tracing::warn!(%error, "session 落库失败");
        }
        drop(recorder);

        // 运行生命周期事件由 AgentStart/AgentEnd 翻译产出：转发任务是广播的
        // 唯一发送方，保证 run 状态与 agent 事件顺序一致。
        if matches!(event, AgentEvent::AgentStart) {
            let _ = session.events.send(ServerEvent::RunStarted {
                session_id: session.id.clone(),
            });
        } else if matches!(event, AgentEvent::AgentEnd { .. }) {
            let _ = session.events.send(ServerEvent::RunFinished {
                session_id: session.id.clone(),
            });
        }
        let _ = session.events.send(ServerEvent::Agent {
            session_id: session.id.clone(),
            event,
        });
    }
}

/// runner 事件转发任务：把 core runner 的 job 生命周期与结果翻译为
/// broadcast 事件。分工（ADR-0033）：
///
/// - prompt / continue 的运行生命周期（RunStarted/RunFinished）仍由
///   agent 事件流的 `AgentStart`/`AgentEnd` 推导（[`forward_events`]，
///   与 agent 事件同通道、顺序一致）；
/// - compact 不产生 `AgentStart`/`AgentEnd`，运行生命周期以 runner 事件
///   合成（Started → RunStarted，Finished → RunFinished）；
/// - 空结果（无可压缩内容 / 无可续跑消息）不产生 agent 事件，就地转为
///   error 事件通知（文案用 core 常量，与 TUI 同一口径）；
/// - 执行失败（agent loop 整体失败时无 `AgentEnd`）补发 RunFinished，
///   避免前端运行状态悬挂。
async fn forward_runner_events(
    session: Arc<SessionRuntime>,
    mut events: mpsc::UnboundedReceiver<RunnerEvent>,
) {
    let send = |event: ServerEvent| {
        let _ = session.events.send(event);
    };
    let run_started = || {
        send(ServerEvent::RunStarted {
            session_id: session.id.clone(),
        });
    };
    let run_finished = || {
        send(ServerEvent::RunFinished {
            session_id: session.id.clone(),
        });
    };
    let notify = |message: String| {
        send(ServerEvent::Error {
            session_id: Some(session.id.clone()),
            request_id: None,
            message,
        });
    };
    while let Some(event) = events.recv().await {
        match event {
            RunnerEvent::Started(JobKind::Compact) => run_started(),
            RunnerEvent::Started(JobKind::Prompt | JobKind::Continue) => {}
            RunnerEvent::Finished(JobOutcome::Prompt(result)) => {
                match result {
                    Ok(outcome) => {
                        // 队列 drain（ADR-0014）：正常结束但队列仍非空
                        //（注入点查询后的滞后入队竞态），弹出队首作为下
                        // 一轮 prompt；异常结束（取消/失败）队列暂停保留
                        if outcome.ended_normally() {
                            drain_queue(&session);
                        }
                    }
                    Err(error) => {
                        tracing::error!(%error, "agent run failed");
                        notify(format!("{error:#}"));
                        run_finished();
                    }
                }
            }
            RunnerEvent::Finished(JobOutcome::Compact(result)) => {
                run_finished();
                match result {
                    // 压缩成功经 CompactionStart/End 事件渲染与落库，无需额外处理
                    Ok(CompactOutcome::Compacted(_)) => {}
                    Ok(CompactOutcome::NothingToCompact) => {
                        notify(NOTHING_TO_COMPACT.to_string());
                    }
                    Err(error) => {
                        tracing::error!(%error, "compact failed");
                        notify(format!("{error:#}"));
                    }
                }
            }
            RunnerEvent::Finished(JobOutcome::Continue(result)) => match result {
                // 续跑成功经事件流渲染与落库，无需额外处理
                Ok(ContinueOutcome::Continued) => {}
                Ok(ContinueOutcome::NothingToContinue) => {
                    notify(NOTHING_TO_CONTINUE.to_string());
                }
                Err(error) => {
                    tracing::error!(%error, "continue failed");
                    notify(format!("{error:#}"));
                    run_finished();
                }
            },
        }
    }
}

/// 队列 drain（ADR-0014）：弹出队首作为下一轮 prompt 提交；runner 串行
/// 消费使各轮 drain 首尾相接成链，直至队列清空。弹出即 mention 展开
///（队列存原文，投递时展开）。
fn drain_queue(session: &Arc<SessionRuntime>) {
    let Some(message) = session.queue.pop_front() else {
        return;
    };
    if let Err(error) = session.runner.submit(SessionJob::Prompt {
        text: message.text,
        images: message.images,
    }) {
        tracing::error!(%error, "queue drain 提交失败");
    }
}

/// 当前状态快照的各部分（api 层拼装成响应）。
pub struct Snapshot {
    pub messages: Vec<Message>,
    pub model: Model,
    pub reasoning: Option<ThinkingLevel>,
    pub context_tokens: u64,
    pub running: bool,
    /// steering 队列内容（原文 + 附件数；前端队列区渲染与编辑寻址用）
    pub queue: Vec<QueueEntryView>,
    pub session: Option<(String, Option<String>)>,
    pub pending_question: Option<(String, AskUserQuestion)>,
    /// 本 session 的 workspace 路径（操作基准）
    pub workspace: PathBuf,
    /// 会话统计信息
    pub stats: nomic_core::SessionStats,
}

/// 收集本 session 的状态快照：agent 查询（经 actor 邮箱）+ 运行时可变状态。
pub async fn snapshot(session: &SessionRuntime) -> Result<Snapshot> {
    let messages = session.handle.messages().await?;
    let model = session.handle.model().await?;
    let reasoning = session.handle.reasoning().await?;
    let context_tokens = session.handle.context_tokens().await?;
    let session_stats = session.handle.stats().await?;
    let running = session.runner.is_running();
    let queue = session.queue.snapshot();
    let title = nomic_session::session_title(&messages);
    let pending_question = session.questions.current();
    Ok(Snapshot {
        messages,
        model,
        reasoning,
        context_tokens,
        running,
        queue,
        session: Some((session.id.clone(), title)),
        pending_question,
        workspace: session.workspace.clone(),
        stats: session_stats,
    })
}

#[cfg(test)]
mod tests {
    //! 队列 drain（ADR-0014）：run 正常结束后队列仍非空（滞后入队竞态），
    //! 队首作为下一轮 prompt 提交；空队列不动。

    #[tokio::test]
    async fn drain_submits_front_as_next_prompt() {
        let (state, session_id) = crate::web::tests::test_state_with_session().await;
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .get(&session_id)
            .expect("session")
            .clone();
        assert!(!session.runner.is_running(), "预置 session 应空闲");

        session.queue.push("下一条".to_string(), Vec::new());
        super::drain_queue(&session);

        assert!(session.queue.snapshot().is_empty(), "drain 应弹出队首");
        assert!(
            session.runner.is_running(),
            "drain 应提交下一轮 prompt（submit 同步入队，无调度点）"
        );
        // 取消在途 job，避免测试发起真实 provider 请求
        session.cancel_run();
    }

    #[tokio::test]
    async fn drain_noop_when_queue_empty() {
        let (state, session_id) = crate::web::tests::test_state_with_session().await;
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .get(&session_id)
            .expect("session")
            .clone();

        super::drain_queue(&session);
        assert!(!session.runner.is_running(), "空队列不应产生 job");
    }
}
