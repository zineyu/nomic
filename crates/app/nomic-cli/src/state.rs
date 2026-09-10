//! 进程级应用状态（ADR-0047）：参考 Axum state 机制的服务注册与传递。
//!
//! bootstrap 装配的全部组件——模型解析器 / provider / session 库 / skills /
//! 提示词配方 / 全局消息总线等——作为服务注册进 [`AppState`]；TUI / print /
//! serve 三入口与其内部组件（driver、工具配方、session 工厂、axum handler）
//! 统一从 state 提取依赖，取代逐字段的长参数列表传递。
//!
//! 对应 Axum 的概念：
//! - [`AppState`] ↔ `State<S>`：单一可 Clone 的状态对象在组件间传递
//!   （`Arc` 共享，克隆廉价）；
//! - 各访问器 ↔ `FromRef<S>` 的子状态提取：组件只取自己需要的服务，
//!   依赖在调用点显式可见。
//!
//! 组件注册即 [`Services`] 的一个字段；新增服务 = 新增字段 + 访问器，
//! 编译器会指出所有需要接线的构造点（类型化 struct 相对 type-map 的
//! 核心收益，见 ADR-0047 的取舍）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nomic_ai::{Message, Model, Provider, StreamOptions};
use nomic_core::CompactionSettings;
use nomic_prompts::PromptTemplate;
use nomic_session::SessionStore;
use nomic_skills::SkillResolver;
use tokio::sync::broadcast;

use crate::model::ModelResolver;
use crate::serve::ServerEvent;
use crate::system_prompt::SystemPromptRecipe;

/// 消息总线容量（broadcast 环形缓冲）：滞后订阅者收 `Lagged` 后重拉快照。
const EVENT_BUS_CAPACITY: usize = 1024;

/// 进程级消息总线：所有 session 的生命周期事件统一发往总线，订阅方
/// （WebSocket 连接、设置变更监听）订阅一次即可接收全部事件（ADR-0033）。
///
/// 事件类型复用 serve 协议的 [`ServerEvent`]；总线注册为 state 服务
/// （ADR-0047），任何拿到 state 的组件都能发布/订阅。需要自持发送端的
/// 常驻组件（session 事件转发、提问 sink、steering 队列）经
/// [`EventBus::sender`] 取副本。
#[derive(Debug, Clone)]
pub struct EventBus {
    sender: broadcast::Sender<ServerEvent>,
}

impl EventBus {
    /// 以默认容量创建总线。
    pub fn new() -> Self {
        Self::with_capacity(EVENT_BUS_CAPACITY)
    }

    /// 以指定容量创建总线（测试用小容量）。
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// 发布事件；无订阅者时丢弃（fire-and-forget，事件流不是可靠通道）。
    pub fn publish(&self, event: ServerEvent) {
        let _ = self.sender.send(event);
    }

    /// 订阅总线：收到自调用时点起的事件；滞后超容量时收 `Lagged`。
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.sender.subscribe()
    }

    /// 原始发送端句柄：需要自持 `Sender` 的组件经此取副本。
    pub fn sender(&self) -> broadcast::Sender<ServerEvent> {
        self.sender.clone()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// 注册进状态的服务集合。
///
/// 构造收在 [`bootstrap`](crate::bootstrap)（唯一的真实装配点）与各
/// 测试夹具；运行期组件只经 [`AppState`] 的访问器读取，不直接触碰字段。
pub struct Services {
    /// 启动模型（`model_configured == false` 时为占位模型）
    pub model: Model,
    /// 启动模型是否来自真实配置（CLI / sqlite）
    pub model_configured: bool,
    /// 运行时模型解析器（候选列表与 api_key 分层，进程级共享）
    pub models: Arc<ModelResolver>,
    pub provider: Arc<dyn Provider>,
    pub stream_options: StreamOptions,
    pub system_prompt: String,
    /// 上下文压缩配置（settings 表 `compaction.*` 合并内置默认）
    pub compaction: CompactionSettings,
    /// session 库句柄；不可用时为 `None`（降级为不持久化）
    pub store: Option<SessionStore>,
    /// `Some((store, session_id))` 时开启落库；web 模式（只开库不预建
    /// session，ADR-0030）恒为 `None`
    pub session: Option<(SessionStore, String)>,
    /// 当前 session 的操作基准（project 严格归属）
    pub project: PathBuf,
    /// resume 恢复的历史消息（新会话为空）
    pub history: Vec<Message>,
    /// 系统提示词配方（project 无关部分）：跨 project 重建时经配方重新生成
    pub prompt_recipe: SystemPromptRecipe,
    pub skill_resolver: SkillResolver,
    /// 可用的 prompt templates（`/name` 调用展开用）
    pub prompt_templates: Vec<PromptTemplate>,
    /// 所有可用模型列表（子 agent 模型选择用）
    pub available_models: Vec<Model>,
    /// 模型别名表（settings 表 `model_aliases` 解析为完整模型；子 agent
    /// 按别名选择模型用）
    pub model_aliases: BTreeMap<String, Model>,
    /// 进程级消息总线
    pub bus: EventBus,
}

/// 进程级应用状态（Axum `State<S>` 的对应物）：全部服务的共享句柄。
///
/// 经 [`AppState::new`] 注册 [`Services`] 后自由克隆传递；组件经访问器
/// 提取依赖（对应 axum handler 的子状态提取）。
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Services>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("model", &self.inner.model)
            .field("model_configured", &self.inner.model_configured)
            .field("store", &self.inner.store)
            .field("project", &self.inner.project)
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// 注册服务集合，返回可共享的状态句柄。
    pub fn new(services: Services) -> Self {
        Self {
            inner: Arc::new(services),
        }
    }

    /// 启动模型。
    pub fn model(&self) -> &Model {
        &self.inner.model
    }

    /// 启动模型是否来自真实配置。
    pub fn model_configured(&self) -> bool {
        self.inner.model_configured
    }

    /// 运行时模型解析器。
    pub fn models(&self) -> &Arc<ModelResolver> {
        &self.inner.models
    }

    /// 启动 provider。
    pub fn provider(&self) -> &Arc<dyn Provider> {
        &self.inner.provider
    }

    /// 流式请求选项。
    pub fn stream_options(&self) -> &StreamOptions {
        &self.inner.stream_options
    }

    /// 系统提示词（已按 project 构建）。
    pub fn system_prompt(&self) -> &str {
        &self.inner.system_prompt
    }

    /// 上下文压缩配置。
    pub fn compaction(&self) -> CompactionSettings {
        self.inner.compaction
    }

    /// session 库句柄。
    pub fn store(&self) -> Option<&SessionStore> {
        self.inner.store.as_ref()
    }

    /// 当前 session 落库句柄（store 与 session id）。
    pub fn session(&self) -> Option<&(SessionStore, String)> {
        self.inner.session.as_ref()
    }

    /// 当前 session 的操作基准（project 路径）。
    pub fn project(&self) -> &Path {
        &self.inner.project
    }

    /// resume 恢复的历史消息。
    pub fn history(&self) -> &[Message] {
        &self.inner.history
    }

    /// 系统提示词配方。
    pub fn prompt_recipe(&self) -> &SystemPromptRecipe {
        &self.inner.prompt_recipe
    }

    /// skill 解析器。
    pub fn skill_resolver(&self) -> &SkillResolver {
        &self.inner.skill_resolver
    }

    /// 可用的 prompt templates。
    pub fn prompt_templates(&self) -> &[PromptTemplate] {
        &self.inner.prompt_templates
    }

    /// 所有可用模型列表。
    pub fn available_models(&self) -> &[Model] {
        &self.inner.available_models
    }

    /// 模型别名表。
    pub fn model_aliases(&self) -> &BTreeMap<String, Model> {
        &self.inner.model_aliases
    }

    /// 进程级消息总线。
    pub fn bus(&self) -> &EventBus {
        &self.inner.bus
    }
}

#[cfg(test)]
impl AppState {
    /// 测试用最小状态：给定的模型与 provider + 空 skills / 无持久化 /
    /// 默认设置（模型解析器与总线真实创建，避免组件空指针分支）。
    pub(crate) fn stub(model: Model, provider: Arc<dyn Provider>) -> Self {
        use clap::Parser as _;

        Self::new(Services {
            available_models: vec![model.clone()],
            model,
            model_configured: true,
            models: Arc::new(ModelResolver::new(
                &crate::Cli::parse_from(["nomic"]),
                crate::settings::Settings::default(),
                None,
                None,
            )),
            provider,
            stream_options: StreamOptions::default(),
            system_prompt: String::new(),
            compaction: CompactionSettings::default(),
            store: None,
            session: None,
            project: PathBuf::from("/repo"),
            history: Vec::new(),
            prompt_recipe: SystemPromptRecipe::default(),
            skill_resolver: SkillResolver::new(
                Path::new("/repo"),
                nomic_skills::ProjectDiscovery::Roots(Vec::new()),
                Vec::new(),
            )
            .expect("empty skill resolver"),
            prompt_templates: Vec::new(),
            model_aliases: BTreeMap::new(),
            bus: EventBus::new(),
        })
    }
}
