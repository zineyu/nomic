//! typestate builder：编译期强制 agent 必填创建项。
//!
//! `model` / `provider` / `system_prompt` 无合理默认值，由幽灵类型标记
//! （[`Set`] / [`Unset`] + [`PhantomData`]）在类型层跟踪设置状态：
//! 每个必填 setter 消费 `self` 并翻转对应类型参数，[`AgentBuilder::build`]
//! 仅在 `AgentBuilder<Set, Set, Set>` 上可用——缺任一必填项则根本无法调用，
//! 类型错误即规格。三个标记相互独立，必填项设置顺序自由。
//!
//! 其余创建项带默认值（与旧调用点手写值一致）：tools/messages 为空、
//! stream_options 为 [`StreamOptions::default`]、hooks 为 [`NoopHooks`]、
//! tool_execution 为 [`ExecutionMode::Parallel`]、compaction 为
//! [`CompactionSettings::default`]。
//!
//! ```
//! # use std::sync::Arc;
//! # use nomic_ai::{Model, Provider};
//! # use nomic_core::Agent;
//! # fn example(model: Model, provider: Arc<dyn Provider>) {
//! let (agent, events) = Agent::builder()
//!     .model(model)
//!     .provider(provider)
//!     .system_prompt("you are helpful")
//!     .build();
//! # }
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use nomic_ai::{Message, Model, Provider, StreamOptions};
use tokio::sync::mpsc;

use crate::AgentEvent;
use crate::agent::{Agent, AgentConfig};
use crate::compaction::CompactionSettings;
use crate::hooks::{AgentHooks, NoopHooks};
use crate::tool::{DynTool, ExecutionMode};

/// 类型状态标记：必填项已设置。
///
/// 仅作为 [`AgentBuilder`] 的类型参数出现，无运行时值。
#[derive(Debug)]
pub struct Set;

/// 类型状态标记：必填项未设置。
///
/// 仅作为 [`AgentBuilder`] 的类型参数出现，无运行时值。
#[derive(Debug)]
pub struct Unset;

/// agent 创建 builder（typestate）。
///
/// 类型参数 `M` / `P` / `S` 分别跟踪 model / provider / system_prompt
/// 的设置状态；经 [`Agent::builder`] 以全 [`Unset`] 起始。
#[must_use]
pub struct AgentBuilder<M = Unset, P = Unset, S = Unset> {
    model: Option<Model>,
    provider: Option<Arc<dyn Provider>>,
    system_prompt: Option<String>,
    tools: Vec<DynTool>,
    messages: Vec<Message>,
    stream_options: StreamOptions,
    hooks: Arc<dyn AgentHooks>,
    tool_execution: ExecutionMode,
    compaction: CompactionSettings,
    _state: PhantomData<(M, P, S)>,
}

impl std::fmt::Debug for AgentBuilder<Set, Set, Set> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentBuilder")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl AgentBuilder<Unset, Unset, Unset> {
    /// 创建空 builder（所有必填项未设置）。
    pub(crate) fn new() -> Self {
        Self {
            model: None,
            provider: None,
            system_prompt: None,
            tools: Vec::new(),
            messages: Vec::new(),
            stream_options: StreamOptions::default(),
            hooks: Arc::new(NoopHooks),
            tool_execution: ExecutionMode::Parallel,
            compaction: CompactionSettings::default(),
            _state: PhantomData,
        }
    }
}

impl<M, P, S> AgentBuilder<M, P, S> {
    /// 设置当前模型（必填）。
    pub fn model(self, model: Model) -> AgentBuilder<Set, P, S> {
        AgentBuilder {
            model: Some(model),
            provider: self.provider,
            system_prompt: self.system_prompt,
            tools: self.tools,
            messages: self.messages,
            stream_options: self.stream_options,
            hooks: self.hooks,
            tool_execution: self.tool_execution,
            compaction: self.compaction,
            _state: PhantomData,
        }
    }

    /// 设置 provider 实现（必填）。
    pub fn provider(self, provider: Arc<dyn Provider>) -> AgentBuilder<M, Set, S> {
        AgentBuilder {
            model: self.model,
            provider: Some(provider),
            system_prompt: self.system_prompt,
            tools: self.tools,
            messages: self.messages,
            stream_options: self.stream_options,
            hooks: self.hooks,
            tool_execution: self.tool_execution,
            compaction: self.compaction,
            _state: PhantomData,
        }
    }

    /// 设置系统提示词（必填）。
    pub fn system_prompt(self, system_prompt: impl Into<String>) -> AgentBuilder<M, P, Set> {
        AgentBuilder {
            model: self.model,
            provider: self.provider,
            system_prompt: Some(system_prompt.into()),
            tools: self.tools,
            messages: self.messages,
            stream_options: self.stream_options,
            hooks: self.hooks,
            tool_execution: self.tool_execution,
            compaction: self.compaction,
            _state: PhantomData,
        }
    }

    /// 设置工具集（默认空）。
    pub fn tools(mut self, tools: Vec<DynTool>) -> Self {
        self.tools = tools;
        self
    }

    /// 设置既有消息历史（默认空；session resume 场景）。
    ///
    /// `messages` 按序作为上下文起点，后续 `prompt` 追加在其后；
    /// 调用方负责保证顺序与来源（如 session store 的 `load_messages` 输出）。
    pub fn messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    /// 设置流式请求选项（默认 [`StreamOptions::default`]）。
    pub fn stream_options(mut self, stream_options: StreamOptions) -> Self {
        self.stream_options = stream_options;
        self
    }

    /// 设置生命周期 hooks（默认 [`NoopHooks`]）。
    pub fn hooks(mut self, hooks: Arc<dyn AgentHooks>) -> Self {
        self.hooks = hooks;
        self
    }

    /// 设置默认工具执行模式（默认 [`ExecutionMode::Parallel`]）。
    pub const fn tool_execution(mut self, tool_execution: ExecutionMode) -> Self {
        self.tool_execution = tool_execution;
        self
    }

    /// 设置上下文压缩配置（默认 [`CompactionSettings::default`]；
    /// `enabled` 只控制自动触发，手动 [`Agent::compact`] 不受限）。
    pub const fn compaction(mut self, compaction: CompactionSettings) -> Self {
        self.compaction = compaction;
        self
    }
}

impl AgentBuilder<Set, Set, Set> {
    /// 创建 agent，返回 agent 本体与事件流的接收端。
    ///
    /// 调用方并发地：驱动 [`Agent::prompt`] 同时从接收端消费事件。
    pub fn build(self) -> (Agent, mpsc::UnboundedReceiver<AgentEvent>) {
        // typestate 保证三个必填项已设置，unreachable 仅存在于类型层之外
        let config = AgentConfig {
            model: self.model.expect("typestate 保证 model 已设置"),
            provider: self.provider.expect("typestate 保证 provider 已设置"),
            stream_options: self.stream_options,
            hooks: self.hooks,
            tool_execution: self.tool_execution,
            compaction: self.compaction,
        };
        let system_prompt = self
            .system_prompt
            .expect("typestate 保证 system_prompt 已设置");
        Agent::from_parts(config, self.tools, system_prompt, self.messages)
    }
}

#[cfg(test)]
mod tests {
    use nomic_ai::{ApiKind, Context, UserMessage, UserMessageContent, now_millis};
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn model() -> Model {
        Model {
            id: "mock-model".to_string(),
            name: "mock".to_string(),
            api: ApiKind::OpenAiCompletions,
            provider: "mock".to_string(),
            base_url: "http://localhost".to_string(),
            reasoning: false,
            context_window: 128_000,
            max_tokens: 4096,
            cost_input: 0.0,
            cost_output: 0.0,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
        }
    }

    /// 最小 provider 实现：构造后即被丢弃，build 路径不发起请求。
    struct MockProvider;

    impl Provider for MockProvider {
        fn stream(
            &self,
            _model: &Model,
            _context: &Context,
            _options: &StreamOptions,
            _cancel: CancellationToken,
        ) -> nomic_ai::AssistantStream {
            unreachable!("build 路径不发起请求")
        }
    }

    #[test]
    fn build_with_required_fields_only_uses_defaults() {
        let (agent, _events) = Agent::builder()
            .model(model())
            .provider(Arc::new(MockProvider))
            .system_prompt("sys")
            .build();
        assert!(agent.messages().is_empty());
    }

    #[test]
    fn messages_seed_history() {
        let history = vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: now_millis(),
        })];
        let (agent, _events) = Agent::builder()
            .provider(Arc::new(MockProvider))
            .system_prompt("sys")
            .model(model())
            .messages(history)
            .build();
        assert_eq!(agent.messages().len(), 1);
    }

    #[test]
    fn required_setters_accept_any_order() {
        // 顺序自由性的正向用例：三种顺序均可编译并产出 agent
        let (a, _) = Agent::builder()
            .model(model())
            .provider(Arc::new(MockProvider))
            .system_prompt("sys")
            .build();
        let (b, _) = Agent::builder()
            .system_prompt("sys")
            .model(model())
            .provider(Arc::new(MockProvider))
            .build();
        let (c, _) = Agent::builder()
            .provider(Arc::new(MockProvider))
            .system_prompt("sys")
            .model(model())
            .build();
        assert!(a.messages().is_empty() && b.messages().is_empty() && c.messages().is_empty());
    }

    /// `/models` 运行时切换：模型替换，历史与工具保留。
    #[test]
    fn set_model_switches_model_keeping_history() {
        let history = vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: now_millis(),
        })];
        let (mut agent, _events) = Agent::builder()
            .model(model())
            .provider(Arc::new(MockProvider))
            .system_prompt("sys")
            .messages(history)
            .build();
        assert_eq!(agent.model().id, "mock-model");

        let next = Model {
            id: "other-model".to_string(),
            context_window: 64_000,
            ..model()
        };
        agent.set_model(next);
        assert_eq!(agent.model().id, "other-model");
        assert_eq!(agent.model().context_window, 64_000);
        assert_eq!(agent.messages().len(), 1, "切换模型保留消息历史");
    }

    #[test]
    fn optional_setters_apply() {
        let compaction = CompactionSettings {
            enabled: false,
            ..CompactionSettings::default()
        };
        let (agent, _events) = Agent::builder()
            .model(model())
            .provider(Arc::new(MockProvider))
            .system_prompt("sys")
            .tool_execution(ExecutionMode::Sequential)
            .compaction(compaction)
            .tools(Vec::new())
            .stream_options(StreamOptions::default())
            .hooks(Arc::new(NoopHooks))
            .build();
        assert!(agent.messages().is_empty());
    }
}
