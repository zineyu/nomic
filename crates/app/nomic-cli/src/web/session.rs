//! 会话构建与事件落库：SessionFactory 按 bootstrap 输入构建每个
//! [`SessionRuntime`]（agent actor + 事件转发任务），
//! 事件转发 / runner / 快照收集收在本模块。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use nomic_ai::{Message, Model, Provider, StreamOptions, ThinkingLevel};
use nomic_core::{Agent, AgentEvent};
use nomic_session::{SessionRecorder, SessionStore};
use nomic_skills::SkillResolver;
use nomic_tools::{AskUserQuestion, TodoStore};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use super::{ServerEvent, SessionRuntime};
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
            return match word.as_str() {
                "minimal" => Some(ThinkingLevel::Minimal),
                "low" => Some(ThinkingLevel::Low),
                "medium" => Some(ThinkingLevel::Medium),
                "high" => Some(ThinkingLevel::High),
                _ => None,
            };
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
        let questions = Arc::new(Mutex::new(HashMap::new()));
        let sink = Arc::new(WebQuestionSink {
            session_id: id.clone(),
            events: events_tx.clone(),
            questions: questions.clone(),
        });
        let (agent, events_rx) = Agent::builder()
            .model(resolved.model)
            .provider(resolved.provider.clone())
            .system_prompt(self.system_prompt.clone())
            .tools({
                // 子 agent 可用的工具池（基础工具，不含管理工具本身）；
                // 主/子 agent 工具都以本 session 的 workspace 为基准
                let child_tools = nomic_tools::default_tools_with_skills_in(
                    Some(workspace.clone()),
                    self.skill_resolver.clone(),
                    TodoStore::new(),
                    sink.clone(),
                );
                // supervisor 管理子 agent 生命周期（per-session）
                let supervisor = Arc::new(nomic_core::AgentSupervisor::new(
                    resolved.provider,
                    self.available_models.clone(),
                    nomic_core::SupervisorConfig::default(),
                ));
                // 主 agent 工具 = 基础工具 + 多 agent 管理工具
                let mut tools = nomic_tools::default_tools_with_skills_in(
                    Some(workspace.clone()),
                    self.skill_resolver.clone(),
                    TodoStore::new(),
                    sink,
                );
                tools.extend(nomic_tools::multi_agent::multi_agent_tools(
                    supervisor,
                    child_tools,
                ));
                tools
            })
            .messages(history)
            .stream_options(resolved.options)
            .compaction(self.compaction)
            .build();
        let (handle, _actor_task) = agent.spawn();

        let session = Arc::new(SessionRuntime {
            id,
            handle,
            recorder: Mutex::new(recorder),
            events: events_tx,
            gate: super::RunGate::new(),
            cancel: Mutex::new(None),
            questions,
            workspace,
        });
        tokio::spawn(forward_events(session.clone(), events_rx));
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

/// runner 任务：串行消费本 session 队列；每轮 prompt 带独立取消令牌，可被
/// cancel 单独中断（中断后队列保留，恢复后继续下一轮）。
pub async fn run_loop(session: &Arc<SessionRuntime>) {
    while let Some(prompt) = session.gate.next().await {
        let cancel = CancellationToken::new();
        *session.cancel.lock().await = Some(cancel.clone());
        let result = session
            .handle
            .prompt_with_images(&prompt.text, &prompt.images, cancel.clone())
            .await;
        *session.cancel.lock().await = None;
        if let Err(error) = result {
            tracing::error!(%error, "agent run failed");
            let _ = session.events.send(ServerEvent::Error {
                session_id: Some(session.id.clone()),
                request_id: None,
                message: format!("{error:#}"),
            });
            // agent loop 整体失败时无 AgentEnd 事件（见 forward_events），
            // 补发 RunFinished 避免前端运行状态悬挂
            let _ = session.events.send(ServerEvent::RunFinished {
                session_id: session.id.clone(),
            });
        }
    }
}

/// 当前状态快照的各部分（api 层拼装成响应）。
pub struct Snapshot {
    pub messages: Vec<Message>,
    pub model: Model,
    pub reasoning: Option<ThinkingLevel>,
    pub context_tokens: u64,
    pub running: bool,
    pub queued: usize,
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
    let (running, queued) = (session.gate.running(), session.gate.len().await);
    let title = nomic_session::session_title(&messages);
    let pending_question = session
        .questions
        .lock()
        .await
        .iter()
        .next()
        .map(|(id, pending)| (id.clone(), pending.question.clone()));
    Ok(Snapshot {
        messages,
        model,
        reasoning,
        context_tokens,
        running,
        queued,
        session: Some((session.id.clone(), title)),
        pending_question,
        workspace: session.workspace.clone(),
        stats: session_stats,
    })
}
