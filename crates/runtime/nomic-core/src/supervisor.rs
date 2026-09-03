//! 多 agent 并行 supervisor（fork-join 模式）。
//!
//! [`AgentSupervisor`] 管理多个独立子 agent 的生命周期：创建、消息发送、
//! 等待结果、关闭。每个子 agent 是独立的 tokio actor（[`AgentHandle`]），
//! 拥有自己的系统提示词、工具集、模型和消息历史。
//!
//! ## 并发模型
//!
//! - `send_message` **非阻塞**：内部 `tokio::spawn` 调用 `handle.prompt()`，
//!   立即返回。子 agent 的 LLM 调用在后台并行执行。
//! - `wait_result` / `wait_all` **阻塞**：取走 `JoinHandle` 并 await。
//! - fork-join 典型流程：创建 N 个 agent → 逐个 `send_message`（全部立即返回）
//!   → `wait_all` 等待全部完成 → 汇总结果。
//!
//! ## 模型选择
//!
//! 子 agent 的模型在创建时按三层解析（ADR-0038）：
//!
//! 1. **别名**：`create_agent` 的 `model` 参数命中别名表（sqlite 设置
//!    `model_aliases`，别名 → 模型）时直接使用对应模型；
//! 2. **模型 ID / `<provider>/<id>`**：在可用模型列表中匹配；
//! 3. **继承**：参数缺省时继承主 agent 的当前模型（[`SharedModel`]
//!    共享单元，主 agent 切换模型时由入口更新，继承始终跟随）。

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::{Arc, RwLock as StdRwLock};

use nomic_ai::{Message, Model, Provider};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::agent::{ActorError, AgentHandle};
use crate::tool::DynTool;
use crate::{Agent, AgentEvent};

// ── 标识 ──────────────────────────────────────────────────────────────

/// 子 agent 的唯一标识（字符串包装，便于 LLM 传参）。
#[derive(Debug, Clone, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentId(pub String);

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ── 创建请求 ──────────────────────────────────────────────────────────

/// 主 agent 当前模型的共享单元（继承语义的载体）。
///
/// supervisor 在「创建子 agent 未指定模型」时读取本单元（继承主 agent
/// 的当前模型）；入口（TUI `/models` 切换、web `switch_model`）在主
/// agent 模型变更时写入本单元，使继承始终跟随主 agent 的最新模型。
/// 锁内只做即时读写、持锁时间可忽略，故用 std 锁而非 tokio 锁。
pub type SharedModel = Arc<StdRwLock<Model>>;

/// 创建主 agent 模型的共享单元（初始值为启动模型）。
pub fn shared_model(initial: Model) -> SharedModel {
    Arc::new(StdRwLock::new(initial))
}

/// 创建子 agent 时的配置。
pub struct CreateAgentRequest {
    /// 自定义 ID（`None` 则自动生成 UUID）。
    pub id: Option<String>,
    /// 系统提示词（必填）。
    pub system_prompt: String,
    /// 该 agent 可用的工具子集（按名称从可用工具池中筛选）。
    pub tools: Vec<DynTool>,
    /// 模型（必填；由用户 / LLM 选择）。
    pub model: Model,
    /// provider 实现（`None` 则继承 supervisor 的默认 provider）。
    pub provider: Option<Arc<dyn Provider>>,
    /// 流式请求选项（`None` 则使用默认）。
    pub stream_options: Option<nomic_ai::StreamOptions>,
}

impl fmt::Debug for CreateAgentRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreateAgentRequest")
            .field("id", &self.id)
            .field("system_prompt", &self.system_prompt)
            .field("model", &self.model)
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

// ── 子 agent 运行时状态 ──────────────────────────────────────────────

/// 一个子 agent 的运行时快照（查询用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentStatus {
    /// 子 agent 的唯一标识。
    pub id: AgentId,
    /// 是否有正在执行的 prompt 任务。
    pub is_running: bool,
    /// 消息历史条数。
    pub message_count: usize,
    /// 模型 ID。
    pub model_id: String,
    /// 系统提示词前 80 字符（摘要展示）。
    pub system_prompt_preview: String,
}

/// 子 agent 的内部状态。
struct ChildAgent {
    /// Actor 句柄（可克隆，用于发送命令）。
    handle: AgentHandle,
    /// Actor 任务的 JoinHandle（关闭时 abort）。
    actor_join: tokio::task::JoinHandle<()>,
    /// 事件接收端（create 后可被 [`AgentSupervisor::take_events`] 取走
    /// 一次——子 agent 落库等观察方接管；不取则随 close 丢弃）。
    events: Option<tokio::sync::mpsc::UnboundedReceiver<AgentEvent>>,
    /// 当前正在执行的 prompt 任务（`send_message` 时 spawn）。
    /// `None` 表示空闲。
    prompt_task: Option<tokio::task::JoinHandle<Result<Vec<Message>, ActorError>>>,
    /// 配置快照（查询用）。
    model_id: String,
    system_prompt: String,
}

// ── 错误 ──────────────────────────────────────────────────────────────

/// supervisor 操作错误。
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// 指定的 agent ID 不存在。
    #[error("agent not found: {0}")]
    NotFound(AgentId),
    /// 该 agent 已有正在执行的 prompt 任务（重复 `send_message`）。
    #[error("agent {0} is already running; wait_result before sending again")]
    AlreadyRunning(AgentId),
    /// 调用了 `wait_result` 但该 agent 没有正在执行的任务。
    #[error("agent {0} has no pending task")]
    NotRunning(AgentId),
    /// actor 任务已退出（panic 或全部句柄断开）。
    #[error("agent {0} actor exited")]
    ActorGone(AgentId),
    /// actor 任务 panic。
    #[error("agent {0} task panicked: {1}")]
    TaskPanicked(AgentId, String),
    /// agent loop 错误（provider 流协议违反）。
    #[error("agent {0} loop error: {1}")]
    LoopError(AgentId, String),
    /// 子 agent 数量已达上限。
    #[error("max agents ({0}) reached")]
    MaxAgentsReached(usize),
    /// 模型标识 / 别名无法解析为已知模型。
    #[error("{0}")]
    UnknownModel(String),
}

// ── 配置 ──────────────────────────────────────────────────────────────

/// supervisor 全局配置。
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// 最大并发子 agent 数量（默认 8）。
    pub max_agents: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self { max_agents: 8 }
    }
}

// ── AgentSupervisor ───────────────────────────────────────────────────

/// 管理所有子 agent 生命周期的 supervisor。
///
/// 内部使用 `RwLock` 保护，支持并发操作（不同 agent 的 `wait_result`
/// 可并发执行）。实际的 agent 逻辑全部在各自的 tokio actor 任务中并行运行。
///
/// 设计为 `Arc<AgentSupervisor>` 共享——多个工具实例持有同一 supervisor。
pub struct AgentSupervisor {
    agents: RwLock<HashMap<AgentId, ChildAgent>>,
    default_provider: Arc<dyn Provider>,
    config: SupervisorConfig,
    /// 可用模型列表（传给工具用于校验和展示）。
    available_models: Vec<Model>,
    /// 模型别名表（别名 → 模型；sqlite 设置 `model_aliases`，创建子
    /// agent 时优先于模型 ID 匹配）。
    aliases: BTreeMap<String, Model>,
    /// 主 agent 的当前模型（子 agent 未指定模型时继承）。
    inherited_model: SharedModel,
}

impl fmt::Debug for AgentSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentSupervisor").finish_non_exhaustive()
    }
}

impl AgentSupervisor {
    /// 创建 supervisor。
    ///
    /// - `default_provider`：子 agent 默认使用的 provider（可在创建时覆盖）。
    /// - `available_models`：可供子 agent 选择的模型列表。
    /// - `aliases`：模型别名表（别名 → 模型；创建子 agent 时按别名选择）。
    /// - `inherited_model`：主 agent 当前模型的共享单元（子 agent 未指定
    ///   模型时继承；入口在主 agent 模型切换时更新）。
    /// - `config`：全局配置（最大 agent 数等）。
    pub fn new(
        default_provider: Arc<dyn Provider>,
        available_models: Vec<Model>,
        aliases: BTreeMap<String, Model>,
        inherited_model: SharedModel,
        config: SupervisorConfig,
    ) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            default_provider,
            config,
            available_models,
            aliases,
            inherited_model,
        }
    }

    /// 可用模型列表（工具展示 / 校验用）。
    pub fn available_models(&self) -> &[Model] {
        &self.available_models
    }

    /// 模型别名表（工具展示用；别名 → 模型）。
    pub const fn aliases(&self) -> &BTreeMap<String, Model> {
        &self.aliases
    }

    /// 主 agent 的当前模型（子 agent 未指定模型时继承）。
    ///
    /// 锁中毒只可能因写入方 panic，此时进程已无健康状态可言，直接 panic。
    #[allow(clippy::missing_panics_doc)]
    pub fn inherited_model(&self) -> Model {
        self.inherited_model
            .read()
            .expect("inherited model lock")
            .clone()
    }

    /// 按标识解析模型：别名 > `<provider>/<模型id>` > 裸模型 id。
    ///
    /// 裸 id 被多个 provider 的同名模型命中时按歧义报错（提示用全形式
    /// 消歧）；全部未命中时报错并列出可用别名与模型。
    pub fn resolve_model(&self, spec: &str) -> Result<Model, SupervisorError> {
        let spec = spec.trim();
        if let Some(model) = self.aliases.get(spec) {
            return Ok(model.clone());
        }
        let matches: Vec<&Model> = match spec.split_once('/') {
            Some((provider, id)) => self
                .available_models
                .iter()
                .filter(|m| m.provider == provider && m.id == id)
                .collect(),
            None => self
                .available_models
                .iter()
                .filter(|m| m.id == spec)
                .collect(),
        };
        match matches.as_slice() {
            [model] => Ok((*model).clone()),
            [] => Err(SupervisorError::UnknownModel(
                self.unknown_model_message(spec),
            )),
            _ => {
                let specs: Vec<String> = matches
                    .iter()
                    .map(|m| format!("{}/{}", m.provider, m.id))
                    .collect();
                Err(SupervisorError::UnknownModel(format!(
                    "ambiguous model \"{spec}\"; use a fully qualified form: [{}]",
                    specs.join(", ")
                )))
            }
        }
    }

    /// 「未知模型」报错文本：列出可用别名与模型（LLM 可据此自纠正）。
    fn unknown_model_message(&self, spec: &str) -> String {
        let aliases: Vec<&str> = self.aliases.keys().map(String::as_str).collect();
        let models: Vec<String> = self
            .available_models
            .iter()
            .map(|m| format!("{}/{}", m.provider, m.id))
            .collect();
        format!(
            "unknown model or alias \"{spec}\"; available aliases: [{}]; available models: [{}]",
            aliases.join(", "),
            models.join(", ")
        )
    }

    /// 创建一个新的子 agent，返回其 ID。
    ///
    /// 子 agent 立即以 actor 模式运行（专属 tokio 任务），但尚无 prompt
    /// 任务——需通过 [`Self::send_message`] 发起。
    pub async fn create(&self, request: CreateAgentRequest) -> Result<AgentId, SupervisorError> {
        let agents = self.agents.read().await;
        if agents.len() >= self.config.max_agents {
            return Err(SupervisorError::MaxAgentsReached(self.config.max_agents));
        }
        drop(agents);

        let id = request.id.map_or_else(
            || AgentId(uuid::Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)).to_string()),
            AgentId,
        );

        let model_id = request.model.id.clone();
        let system_prompt = request.system_prompt.clone();

        let provider = request
            .provider
            .unwrap_or_else(|| self.default_provider.clone());

        let mut builder = Agent::builder()
            .model(request.model)
            .provider(provider)
            .system_prompt(&request.system_prompt)
            .tools(request.tools);

        if let Some(opts) = request.stream_options {
            builder = builder.stream_options(opts);
        }

        let (agent, events) = builder.build();
        let (handle, actor_join) = agent.spawn();

        let child = ChildAgent {
            handle,
            actor_join,
            events: Some(events),
            prompt_task: None,
            model_id,
            system_prompt,
        };

        self.agents.write().await.insert(id.clone(), child);
        tracing::info!(agent_id = %id, "child agent created");
        Ok(id)
    }

    /// 取走子 agent 的事件接收端（仅一次；已被取走或 id 不存在时返回
    /// `None`）。子 agent 落库（ADR-0044：子 agent 记为同 work 下的
    /// session）等观察方在 create 后立即接管事件流。
    pub async fn take_events(
        &self,
        id: &AgentId,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<AgentEvent>> {
        self.agents.write().await.get_mut(id)?.events.take()
    }

    /// 向指定 agent 发送消息（**非阻塞**）。
    ///
    /// 内部 `tokio::spawn` 调用 `handle.prompt()`，立即返回。子 agent 的
    /// LLM 调用在后台并行执行。调用 [`Self::wait_result`] 收取结果。
    ///
    /// 如果该 agent 已有正在执行的 prompt 任务，返回
    /// [`SupervisorError::AlreadyRunning`]。
    pub async fn send_message(
        &self,
        id: &AgentId,
        text: &str,
        cancel: CancellationToken,
    ) -> Result<(), SupervisorError> {
        let mut agents = self.agents.write().await;
        let child = agents
            .get_mut(id)
            .ok_or_else(|| SupervisorError::NotFound(id.clone()))?;

        if child.prompt_task.is_some() {
            return Err(SupervisorError::AlreadyRunning(id.clone()));
        }

        let handle = child.handle.clone();
        let text = text.to_string();
        let agent_id = id.clone();
        let task = tokio::spawn(async move {
            tracing::debug!(agent_id = %agent_id, "child prompt started");
            let result = handle.prompt(&text, cancel).await;
            tracing::debug!(agent_id = %agent_id, "child prompt finished");
            result
        });
        child.prompt_task = Some(task);
        drop(agents);
        Ok(())
    }

    /// 等待指定 agent 当前 prompt 完成（**阻塞**），返回 assistant 消息。
    ///
    /// 取走并 await 子 agent 的 prompt `JoinHandle`。如果该 agent 没有
    /// 正在执行的任务，返回 [`SupervisorError::NotRunning`]。
    pub async fn wait_result(&self, id: &AgentId) -> Result<Vec<Message>, SupervisorError> {
        let task = {
            let mut agents = self.agents.write().await;
            let child = agents
                .get_mut(id)
                .ok_or_else(|| SupervisorError::NotFound(id.clone()))?;
            let task = child.prompt_task.take();
            drop(agents);
            task
        };

        let task = task.ok_or_else(|| SupervisorError::NotRunning(id.clone()))?;

        match task.await {
            Ok(Ok(messages)) => Ok(messages),
            Ok(Err(ActorError::Gone)) => Err(SupervisorError::ActorGone(id.clone())),
            Ok(Err(ActorError::Loop(err))) => {
                Err(SupervisorError::LoopError(id.clone(), err.to_string()))
            }
            Ok(Err(ActorError::Compaction(err))) => {
                Err(SupervisorError::LoopError(id.clone(), err.to_string()))
            }
            Err(join_err) => {
                if join_err.is_cancelled() {
                    Err(SupervisorError::TaskPanicked(
                        id.clone(),
                        "cancelled".to_string(),
                    ))
                } else {
                    Err(SupervisorError::TaskPanicked(
                        id.clone(),
                        join_err.to_string(),
                    ))
                }
            }
        }
    }

    /// 等待多个 agent 全部完成（**阻塞**），返回每个 agent 的 assistant 消息。
    ///
    /// 内部并发 await 所有 `JoinHandle`，总耗时 = 最慢 agent 的耗时。
    /// 任何一个 agent 出错则返回该错误（已完成的 agent 结果丢弃）。
    pub async fn wait_all(
        &self,
        ids: &[AgentId],
    ) -> Result<HashMap<AgentId, Vec<Message>>, SupervisorError> {
        // 取出所有 JoinHandle（释放写锁后 await）
        let tasks: Vec<(AgentId, tokio::task::JoinHandle<_>)> = {
            let mut agents = self.agents.write().await;
            ids.iter()
                .map(|id| {
                    let child = agents
                        .get_mut(id)
                        .ok_or_else(|| SupervisorError::NotFound(id.clone()))?;
                    let task = child
                        .prompt_task
                        .take()
                        .ok_or_else(|| SupervisorError::NotRunning(id.clone()))?;
                    Ok((id.clone(), task))
                })
                .collect::<Result<Vec<_>, SupervisorError>>()?
        };

        // 并发 await
        let futures = tasks.into_iter().map(|(id, task)| async move {
            let result = match task.await {
                Ok(Ok(messages)) => Ok(messages),
                Ok(Err(ActorError::Gone)) => Err(SupervisorError::ActorGone(id.clone())),
                Ok(Err(err)) => Err(SupervisorError::LoopError(id.clone(), err.to_string())),
                Err(join_err) => Err(SupervisorError::TaskPanicked(
                    id.clone(),
                    join_err.to_string(),
                )),
            };
            (id, result)
        });

        let results = futures::future::join_all(futures).await;
        let mut map = HashMap::new();
        for (id, result) in results {
            map.insert(id, result?);
        }
        Ok(map)
    }

    /// 关闭指定 agent（abort 其 actor 任务，释放资源）。
    pub async fn close(&self, id: &AgentId) -> Result<(), SupervisorError> {
        let child = {
            let mut agents = self.agents.write().await;
            agents
                .remove(id)
                .ok_or_else(|| SupervisorError::NotFound(id.clone()))?
        };
        // 先 abort prompt 任务（如有），再 abort actor 任务
        if let Some(task) = child.prompt_task {
            task.abort();
        }
        child.actor_join.abort();
        tracing::info!(agent_id = %id, "child agent closed");
        Ok(())
    }

    /// 关闭所有子 agent。
    pub async fn close_all(&self) {
        let mut agents = self.agents.write().await;
        for (id, child) in agents.drain() {
            if let Some(task) = child.prompt_task {
                task.abort();
            }
            child.actor_join.abort();
            tracing::info!(agent_id = %id, "child agent closed (close_all)");
        }
    }

    /// 查询指定 agent 的状态。
    pub async fn status(&self, id: &AgentId) -> Result<AgentStatus, SupervisorError> {
        let (is_running, model_id, system_prompt_preview, handle) = {
            let agents = self.agents.read().await;
            let child = agents
                .get(id)
                .ok_or_else(|| SupervisorError::NotFound(id.clone()))?;
            let result = (
                child.prompt_task.is_some(),
                child.model_id.clone(),
                truncate_str(&child.system_prompt, 80),
                child.handle.clone(),
            );
            drop(agents);
            result
        };
        let message_count = handle.messages().map_or(0, |m| m.len());

        Ok(AgentStatus {
            id: id.clone(),
            is_running,
            message_count,
            model_id,
            system_prompt_preview,
        })
    }

    /// 列出所有子 agent 的状态。
    pub async fn list(&self) -> Vec<AgentStatus> {
        let children: Vec<(AgentId, bool, String, String, AgentHandle)> = {
            let agents = self.agents.read().await;
            agents
                .iter()
                .map(|(id, child)| {
                    (
                        id.clone(),
                        child.prompt_task.is_some(),
                        child.model_id.clone(),
                        truncate_str(&child.system_prompt, 80),
                        child.handle.clone(),
                    )
                })
                .collect()
        };
        let mut statuses = Vec::with_capacity(children.len());
        for (id, is_running, model_id, system_prompt_preview, handle) in children {
            let message_count = handle.messages().map_or(0, |m| m.len());
            statuses.push(AgentStatus {
                id,
                is_running,
                message_count,
                model_id,
                system_prompt_preview,
            });
        }
        statuses
    }

    /// 当前子 agent 数量。
    pub async fn count(&self) -> usize {
        self.agents.read().await.len()
    }
}

/// 截断字符串到指定长度，超出部分用 `…` 替代。
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut index = max_len;
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    format!("{}…", &s[..index])
}
