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
use nomic_core::{AgentBuilder, AgentSupervisor, DynTool, SupervisorConfig, TurnInjection};
use nomic_skills::SkillResolver;
use nomic_tools::{BaseDir, QuestionSink, TodoStore};

/// 子 agent 池的 todo 清单策略。
///
/// 语义差异是入口级的：清单是 agent 的工作记忆，是否让子 agent 与主
/// agent 看到同一份，取决于入口是否有跨 agent 的进度观察方。
#[derive(Debug, Clone)]
pub enum TodoPolicy {
    /// 主/子 agent 共享同一份清单（TUI：goal 模式与界面经共享句柄观察
    /// 进度，子 agent 写入的任务在同一清单中可见）。
    Shared(TodoStore),
    /// 主/子 agent 各自新建独立清单（print / web：清单是各 agent 私有的
    /// 工作记忆，跨 agent 不可见）。
    Isolated,
}

/// 组装选项：三入口的差异点全部在此显式表达。
///
/// supervisor 配置（[`SupervisorConfig::default`]）刻意**不是**选项——
/// 三入口当前一致，属于配方本身；某入口需要分化时再提升为选项。
pub struct RecipeOpts {
    /// 工具的相对路径基准句柄（workspace 严格归属）。交互端保留句柄
    /// 副本，session 切换时经 [`BaseDir::set`] 原地更新，已构建工具的
    /// 下一次执行即读到新基准；print / web 入口新建后不再更新。
    pub base: BaseDir,
    /// `skill://` 解析器（注入 read 工具；主/子 agent 共用同一目录）。
    pub skill_resolver: SkillResolver,
    /// `ask_user_question` 的提问通道适配器（入口各自实现：TUI 弹层 /
    /// stdin / web 事件总线）；主/子 agent 共享同一适配器。
    pub question_sink: Arc<dyn QuestionSink>,
    /// todo 清单策略（共享 vs 独立，见 [`TodoPolicy`]）。
    pub todo: TodoPolicy,
    /// 子 agent 的默认 provider（supervisor 持有，创建子 agent 时可逐个
    /// 覆盖；主 agent 的 provider 由调用方直接交给 builder，二者通常相同）。
    pub provider: Arc<dyn Provider>,
    /// 可供子 agent 选择的模型列表（supervisor 校验与展示用）。
    pub available_models: Vec<Model>,
    /// 运行中注入源（ADR-0014，仅 TUI：自持统一消息队列，core 在 turn
    /// 边界经注入点弹出注入；非交互入口为 `None`）。
    pub turn_injection: Option<Arc<dyn TurnInjection>>,
}

/// 组装产物：主 agent 工具集 + 可选注入点。
///
/// 经 [`AgentRecipe::apply`] 装到 agent builder 上；supervisor 生命周期
/// 由管理工具内部持有的 `Arc` 维持，调用方无需再接触。
pub struct AgentRecipe {
    tools: Vec<DynTool>,
    turn_injection: Option<Arc<dyn TurnInjection>>,
}

/// 按配方组装：主 agent 工具 = 基础工具（含 skills、todo、提问）+
/// 多 agent 管理工具；子 agent 池 = 同构的基础工具。
pub fn assemble(opts: RecipeOpts) -> AgentRecipe {
    let (main_todo, child_todo) = match opts.todo {
        TodoPolicy::Shared(store) => (store.clone(), store),
        TodoPolicy::Isolated => (TodoStore::new(), TodoStore::new()),
    };
    // 子 agent 可用的工具池（基础工具，不含管理工具本身）
    let child_tools = nomic_tools::default_tools_with_skills_in_shared(
        &opts.base,
        opts.skill_resolver.clone(),
        child_todo,
        opts.question_sink.clone(),
    );
    // supervisor 管理子 agent 生命周期
    let supervisor = Arc::new(AgentSupervisor::new(
        opts.provider,
        opts.available_models,
        SupervisorConfig::default(),
    ));
    // 主 agent 工具 = 基础工具 + 多 agent 管理工具
    let mut tools = nomic_tools::default_tools_with_skills_in_shared(
        &opts.base,
        opts.skill_resolver,
        main_todo,
        opts.question_sink,
    );
    tools.extend(nomic_tools::multi_agent::multi_agent_tools(
        supervisor,
        child_tools,
    ));
    AgentRecipe {
        tools,
        turn_injection: opts.turn_injection,
    }
}

impl AgentRecipe {
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
    use std::path::Path;

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
        RecipeOpts {
            base: BaseDir::new(None),
            skill_resolver: SkillResolver::new(
                Path::new("/repo"),
                nomic_skills::ProjectDiscovery::Roots(Vec::new()),
                Vec::new(),
            )
            .expect("empty skill resolver"),
            question_sink: Arc::new(NoopSink),
            todo,
            provider: Arc::new(MockProvider),
            available_models: Vec::new(),
            turn_injection: None,
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

    /// 共享策略：主 agent 工具写入的 todo 经共享句柄可见（TUI goal
    /// 模式与界面观察的正是这份清单）。
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
