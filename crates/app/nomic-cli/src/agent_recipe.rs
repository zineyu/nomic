//! agent 配方组装：主/子 agent 工具集与 supervisor 的统一装配点。
//!
//! 配方（多 agent 形态见 ADR-0031）：**主 agent = 基础工具 + supervisor
//! 管理工具；子 agent 池 = 基础工具**（不含管理工具，避免子 agent 递归
//! 创建子 agent）。此前该配方以几乎相同的代码内联在 TUI / print / web
//! 三入口，各点的差异（todo 清单共享与否、sink 适配器、turn 注入点）
//! 承载语义却不可见，新增工具或调整配方需三处协调修改。
//!
//! 本模块把配方收进 [`assemble`]：入口差异经 [`RecipeOpts`] 显式传入，
//! 不变部分——基础工具清单、supervisor 配置、工具池接线——成为内部
//! 细节。nomic-tools 的多个 `default_tools*` 构造变体在此收敛为唯一的
//! 共享基准句柄形式：入口只需给出 [`BaseDir`]（不需要原地更新的入口
//! 新建后不再写它即可，行为等同按固定路径构建）。

use std::sync::Arc;

use nomic_ai::{Model, Provider};
use nomic_core::{
    AgentBuilder, AgentSupervisor, DynTool, SharedModel, SupervisorConfig, TurnInjection,
    shared_model,
};
use nomic_session::SessionStore;
use nomic_tools::multi_agent::ChildSessionHook;
use nomic_tools::{BaseDir, QuestionSink, TodoStore};

use crate::state::AppState;

/// 子 agent 池的 todo 清单策略。
///
/// 语义差异是入口级的：清单是 agent 的工作记忆，是否让子 agent 与主
/// agent 看到同一份，取决于入口是否有跨 agent 的进度观察方。
#[derive(Debug, Clone)]
pub enum TodoPolicy {
    /// 主/子 agent 共享同一份清单（TUI：子 agent 写入的任务与主 agent
    /// 同清单，跨 agent 协作时进度互见）。
    Shared(TodoStore),
    /// 主/子 agent 各自新建独立清单（print / web：清单是各 agent 私有的
    /// 工作记忆，跨 agent 不可见）。
    Isolated,
}

/// 子 agent 落库配置（ADR-0044）：子 agent 的对话落库为父 session 所属
/// work 下的子 session（`parent_session_id` 记血缘，只读回溯）。
/// `None`（无持久化的运行）时子 agent 对话不落库。
#[derive(Debug, Clone)]
pub struct ChildSessionSpec {
    /// session 库句柄
    pub store: SessionStore,
    /// 父 session（当前主 session）id：创建子 session 时记血缘
    pub parent_session_id: String,
}

/// 组装选项：三入口的差异点全部在此显式表达。
///
/// 进程级共享服务（skill 解析器 / 子 agent 候选模型与别名表）经
/// [`AppState`] 提取（ADR-0047）；本 struct 只保留入口间与 session 间
/// 真正分化的输入。supervisor 配置（[`SupervisorConfig::default`]）刻意
/// **不是**选项——三入口当前一致，属于配方本身；某入口需要分化时再提升
/// 为选项。
pub struct RecipeOpts {
    /// 进程级应用状态：skill 解析器、子 agent 候选模型列表与模型别名表
    /// 的服务来源
    pub state: AppState,
    /// 工具的相对路径基准句柄（project 严格归属）。交互端保留句柄
    /// 副本，session 切换时经 [`BaseDir::set`] 原地更新，已构建工具的
    /// 下一次执行即读到新基准；print / web 入口新建后不再更新。
    pub base: BaseDir,
    /// `ask_user_question` 的提问通道适配器（入口各自实现：TUI 弹层 /
    /// stdin / web 事件总线）；主/子 agent 共享同一适配器。
    pub question_sink: Arc<dyn QuestionSink>,
    /// todo 清单策略（共享 vs 独立，见 [`TodoPolicy`]）。
    pub todo: TodoPolicy,
    /// 子 agent 的默认 provider（supervisor 持有，创建子 agent 时可逐个
    /// 覆盖；主 agent 的 provider 由调用方直接交给 builder，二者通常相同。
    /// serve 按 session 解析后传入，可能与进程默认不同）。
    pub provider: Arc<dyn Provider>,
    /// 主 agent 的当前模型：子 agent 未指定模型时继承（ADR-0038）；交互端
    /// 在主 agent 模型切换时经 [`AgentRecipe::inherited_model_cell`] 更新。
    /// serve 按 session 解析后传入，可能与进程默认不同。
    pub default_model: Model,
    /// 运行中注入源（ADR-0014，交互端自持统一消息队列，core 在 turn
    /// 边界经注入点弹出注入；非交互入口为 `None`）。
    pub turn_injection: Option<Arc<dyn TurnInjection>>,
    /// 子 agent 落库（ADR-0044）；无持久化的入口为 `None`。
    pub child_sessions: Option<ChildSessionSpec>,
}

/// 组装产物：主 agent 工具集 + 可选注入点。
///
/// 经 [`AgentRecipe::apply`] 装到 agent builder 上；supervisor 生命周期
/// 由管理工具内部持有的 `Arc` 维持，调用方无需再接触。
pub struct AgentRecipe {
    tools: Vec<DynTool>,
    turn_injection: Option<Arc<dyn TurnInjection>>,
    /// 主 agent 模型的共享单元（子 agent 继承语义的载体）：supervisor 读、
    /// 入口在主 agent 模型切换时写；`apply` 消耗配方前经
    /// [`AgentRecipe::inherited_model_cell`] 取走句柄。
    inherited_model: SharedModel,
}

/// 按配方组装：主 agent 工具 = 基础工具（含 skills、todo、提问）+
/// 多 agent 管理工具；子 agent 池 = 同构的基础工具。
pub fn assemble(opts: RecipeOpts) -> AgentRecipe {
    tracing::debug!(
        todo_policy = match &opts.todo {
            TodoPolicy::Shared(_) => "shared",
            TodoPolicy::Isolated => "isolated",
        },
        has_injection = opts.turn_injection.is_some(),
        "assembling agent recipe"
    );
    let (main_todo, child_todo) = match opts.todo {
        TodoPolicy::Shared(store) => (store.clone(), store),
        TodoPolicy::Isolated => (TodoStore::new(), TodoStore::new()),
    };
    let state = &opts.state;
    // 子 agent 可用的工具池（基础工具，不含管理工具本身）
    let child_tools = nomic_tools::default_tools_with_skills_in_shared(
        &opts.base,
        state.skill_resolver().clone(),
        child_todo,
        opts.question_sink.clone(),
    );
    // supervisor 管理子 agent 生命周期；继承模型单元与入口共享（主 agent
    // 切换模型时更新，子 agent 的继承始终跟随主 agent 当前模型）
    let inherited_model = shared_model(opts.default_model);
    let supervisor = Arc::new(AgentSupervisor::new(
        opts.provider,
        state.available_models().to_vec(),
        state.model_aliases().clone(),
        inherited_model.clone(),
        SupervisorConfig::default(),
    ));
    // 子 agent 落库接缝（ADR-0044）：create_agent 成功后接管子 agent
    // 事件流——在父 session 所属 work 下创建子 session（记血缘），spawn
    // 落库任务消费事件至通道关闭（子 agent close / 进程退出）
    let child_hook: Option<ChildSessionHook> =
        opts.child_sessions.map(|spec| -> ChildSessionHook {
            Arc::new(
                move |agent_id: nomic_core::AgentId,
                      events: tokio::sync::mpsc::UnboundedReceiver<nomic_core::AgentEvent>| {
                    let store = spec.store.clone();
                    let parent = spec.parent_session_id.clone();
                    tokio::spawn(async move {
                        let work = match store.work_of_session(&parent).await {
                            Ok(Some(work)) => work,
                            other => {
                                tracing::warn!(
                                    ?other,
                                    parent,
                                    "child session: work lookup failed"
                                );
                                return;
                            }
                        };
                        match store.create_session_in_work(&work.id, Some(&parent)).await {
                            Ok(child_session_id) => {
                                tracing::info!(
                                    agent_id = %agent_id.0,
                                    child_session_id,
                                    work_id = %work.id,
                                    "child agent session created"
                                );
                                let mut recorder =
                                    nomic_session::SessionRecorder::new(store, child_session_id);
                                let mut events = events;
                                while let Some(event) = events.recv().await {
                                    if let Err(error) = recorder.record(&event).await {
                                        tracing::warn!(?error, "child session record failed");
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::warn!(?error, "failed to create child agent session");
                            }
                        }
                    });
                },
            )
        });
    // 主 agent 工具 = 基础工具 + 多 agent 管理工具
    let mut tools = nomic_tools::default_tools_with_skills_in_shared(
        &opts.base,
        state.skill_resolver().clone(),
        main_todo,
        opts.question_sink,
    );
    tools.extend(nomic_tools::multi_agent::multi_agent_tools(
        supervisor,
        child_tools,
        child_hook,
    ));
    tracing::debug!(total_tools = tools.len(), "agent recipe assembled");
    AgentRecipe {
        tools,
        turn_injection: opts.turn_injection,
        inherited_model,
    }
}

impl AgentRecipe {
    /// 主 agent 工具集（`DynTool` 是 `Arc` 共享句柄，克隆廉价）：交互端
    /// 运行期整体替换工具集（goal 模式换入换出）时以它为正常态基准。
    pub fn tools(&self) -> &[DynTool] {
        &self.tools
    }

    /// 主 agent 模型的共享单元（继承句柄）：交互端在主 agent 模型切换时
    /// 写入（`*cell.write() = new_model`），此后创建的未指定模型的子
    /// agent 继承新模型。`apply` 消耗配方，需在调用前取走本句柄。
    pub fn inherited_model_cell(&self) -> SharedModel {
        self.inherited_model.clone()
    }

    /// 把产物装到 agent builder 上：设置工具集；有注入点则一并设置。
    ///
    /// tools / turn_injection 均非 typestate 必填项，`apply` 不改变
    /// builder 的类型状态，可在 builder 链的任意位置插入。
    pub fn apply<M, P, S>(self, builder: AgentBuilder<M, P, S>) -> AgentBuilder<M, P, S> {
        let builder = builder.tools(self.tools);
        match self.turn_injection {
            Some(injection) => builder.turn_injection(injection),
            None => builder,
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use nomic_ai::{Context, StreamOptions};
    use nomic_core::{Agent, ToolError, TurnMessage};
    use nomic_tools::{AskUserAnswer, AskUserQuestion};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    struct MockProvider;

    impl Provider for MockProvider {
        fn stream(
            &self,
            _model: &Model,
            _context: &Context,
            _options: &StreamOptions,
            _cancel: CancellationToken,
        ) -> nomic_ai::AssistantStream {
            unreachable!("assemble 不发起请求")
        }
    }

    struct NoopSink;

    #[async_trait]
    impl QuestionSink for NoopSink {
        async fn ask(
            &self,
            _question: AskUserQuestion,
            _cancel: CancellationToken,
        ) -> anyhow::Result<AskUserAnswer, ToolError> {
            unreachable!("测试不提问")
        }
    }

    struct NoopInjection;

    impl TurnInjection for NoopInjection {
        fn next_message(&self) -> Option<TurnMessage> {
            None
        }
    }

    fn opts(todo: TodoPolicy) -> RecipeOpts {
        let model = Model {
            id: "mock".to_string(),
            name: "mock".to_string(),
            api: nomic_ai::ApiKind::OpenAiCompletions,
            provider: "mock".to_string(),
            base_url: "http://localhost".to_string(),
            reasoning: false,
            vision: false,
            context_window: 128_000,
            max_tokens: 4096,
            cost_input: 0.0,
            cost_output: 0.0,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
        };
        let provider: Arc<dyn Provider> = Arc::new(MockProvider);
        RecipeOpts {
            state: AppState::stub(model.clone(), provider.clone()),
            base: BaseDir::new(None),
            question_sink: Arc::new(NoopSink),
            todo,
            provider,
            default_model: model,
            turn_injection: None,
            child_sessions: None,
        }
    }

    fn tool_names(recipe: &AgentRecipe) -> Vec<&'static str> {
        recipe.tools.iter().map(DynTool::name).collect()
    }

    /// 配方：主 agent = 9 个基础工具 + 6 个多 agent 管理工具，顺序稳定
    ///（基础在前、管理在后）。
    #[test]
    fn assemble_composes_base_plus_multi_agent_tools() {
        let recipe = assemble(opts(TodoPolicy::Isolated));
        assert_eq!(
            tool_names(&recipe),
            vec![
                "read",
                "write",
                "edit",
                "bash",
                "grep",
                "find",
                "todo_read",
                "todo_write",
                "ask_user_question",
                "create_agent",
                "send_message",
                "wait_result",
                "wait_all",
                "close_agent",
                "list_agents",
            ]
        );
    }

    async fn write_todo(recipe: &AgentRecipe, title: &str) {
        let tool = recipe
            .tools
            .iter()
            .find(|tool| tool.name() == "todo_write")
            .expect("主工具集含 todo_write");
        tool.execute(
            json!({"todos": [{"title": title, "status": "pending", "children": []}]}),
            CancellationToken::new(),
            Box::new(|_| {}),
        )
        .await
        .expect("todo_write 不应失败");
    }

    /// 共享策略：主 agent 工具写入的 todo 经共享句柄可见（主子同清单）。
    #[tokio::test]
    async fn shared_todo_store_observable_via_handle() {
        let store = TodoStore::new();
        let recipe = assemble(opts(TodoPolicy::Shared(store.clone())));
        write_todo(&recipe, "shared task").await;
        assert_eq!(store.todos().len(), 1);
        assert_eq!(store.todos()[0].title, "shared task");
    }

    /// 独立策略：两次组装的清单互不可见（print / web 语义——清单是
    /// 各 agent 私有的工作记忆）。
    #[tokio::test]
    async fn isolated_todo_stores_are_independent() {
        let recipe_a = assemble(opts(TodoPolicy::Isolated));
        let recipe_b = assemble(opts(TodoPolicy::Isolated));
        write_todo(&recipe_a, "a's task").await;

        let read_b = recipe_b
            .tools
            .iter()
            .find(|tool| tool.name() == "todo_read")
            .expect("主工具集含 todo_read");
        let result = read_b
            .execute(json!({}), CancellationToken::new(), Box::new(|_| {}))
            .await
            .expect("todo_read 不应失败");
        let [nomic_ai::UserContent::Text(text)] = &result.content[..] else {
            panic!("expected text result");
        };
        assert!(
            !text.text.contains("a's task"),
            "独立清单不应看到别的 agent 的任务：{}",
            text.text
        );
    }

    /// apply 不改变 typestate：必填项可在 apply 前后自由设置，注入点
    /// 为 None 时等价于不设置。
    #[test]
    fn apply_keeps_builder_typestate_free() {
        let model = Model {
            id: "mock".to_string(),
            name: "mock".to_string(),
            api: nomic_ai::ApiKind::OpenAiCompletions,
            provider: "mock".to_string(),
            base_url: "http://localhost".to_string(),
            reasoning: false,
            vision: false,
            context_window: 128_000,
            max_tokens: 4096,
            cost_input: 0.0,
            cost_output: 0.0,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
        };
        // 注入点 None：apply 插在必填项之后
        let recipe = assemble(opts(TodoPolicy::Isolated));
        let (agent, _events) = recipe
            .apply(
                Agent::builder()
                    .model(model.clone())
                    .provider(Arc::new(MockProvider))
                    .system_prompt("sys"),
            )
            .build();
        assert!(agent.messages().is_empty());
        // 注入点 Some：apply 插在必填项中间（顺序自由的逆向用例）
        let mut opts = opts(TodoPolicy::Isolated);
        opts.turn_injection = Some(Arc::new(NoopInjection));
        let recipe = assemble(opts);
        let builder = Agent::builder()
            .model(model)
            .provider(Arc::new(MockProvider));
        let (agent, _events) = recipe.apply(builder).system_prompt("sys").build();
        assert!(agent.messages().is_empty());
    }
}
