//! create_agent 的模型解析集成测试：继承主 agent 模型 / 别名 / 全形式 /
//! 未知标识报错；以及工具描述中的别名与能力标签展示。

use std::collections::BTreeMap;
use std::sync::Arc;

use nomic_ai::{ApiKind, Context, Model, Provider, StreamOptions};
use nomic_core::{AgentSupervisor, DynTool, SupervisorConfig, shared_model};
use nomic_tools::multi_agent::CreateAgentTool;
use tokio_util::sync::CancellationToken;

struct MockProvider;

impl Provider for MockProvider {
    fn stream(
        &self,
        _model: &Model,
        _context: &Context,
        _options: &StreamOptions,
        _cancel: CancellationToken,
    ) -> nomic_ai::AssistantStream {
        unreachable!("create_agent 不发起 LLM 请求")
    }
}

fn model(provider: &str, id: &str, reasoning: bool, vision: bool) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: ApiKind::OpenAiCompletions,
        provider: provider.to_string(),
        base_url: "http://localhost".to_string(),
        reasoning,
        vision,
        context_window: 128_000,
        max_tokens: 4096,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    }
}

/// 装配 supervisor：主 agent 模型 mock/main；别名 smart → anthropic/opus；
/// 可用模型含 opus 与 mini。
fn supervisor() -> Arc<AgentSupervisor> {
    let opus = model("anthropic", "opus", true, true);
    let mini = model("openai", "mini", false, false);
    Arc::new(AgentSupervisor::new(
        Arc::new(MockProvider),
        vec![opus.clone(), mini],
        BTreeMap::from([("smart".to_string(), opus)]),
        shared_model(model("mock", "main", false, false)),
        SupervisorConfig::default(),
    ))
}

fn create_tool(supervisor: Arc<AgentSupervisor>) -> DynTool {
    DynTool::new(CreateAgentTool::new(supervisor, Vec::new()))
}

async fn create_agent(tool: &DynTool, model: Option<&str>) -> Result<String, String> {
    let params = match model {
        Some(model) => serde_json::json!({"model": model, "system_prompt": "sys"}),
        None => serde_json::json!({"system_prompt": "sys"}),
    };
    tool.execute(params, CancellationToken::new(), Box::new(|_| {}))
        .await
        .map(|result| {
            let [nomic_ai::UserContent::Text(text)] = &result.content[..] else {
                panic!("expected text result");
            };
            text.text.clone()
        })
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn omitted_model_inherits_main_agent_model() {
    let sup = supervisor();
    let tool = create_tool(sup.clone());
    let text = create_agent(&tool, None).await.expect("create 应成功");
    assert!(text.contains("Model: main"), "{text}");
    assert!(text.contains("inherited from main agent"), "{text}");
    // supervisor 中的子 agent 快照同为继承模型
    let status = &sup.list().await[0];
    assert_eq!(status.model_id, "main");
}

#[tokio::test]
async fn alias_resolves_to_aliased_model() {
    let sup = supervisor();
    let tool = create_tool(sup.clone());
    let text = create_agent(&tool, Some("smart"))
        .await
        .expect("alias 应命中");
    assert!(text.contains("Model: opus"), "{text}");
    assert!(text.contains("alias \"smart\""), "{text}");
    let status = &sup.list().await[0];
    assert_eq!(status.model_id, "opus");
}

#[tokio::test]
async fn qualified_and_bare_model_id_resolve() {
    let sup = supervisor();
    let tool = create_tool(sup.clone());
    let text = create_agent(&tool, Some("anthropic/opus"))
        .await
        .expect("全形式应命中");
    assert!(text.contains("Model: opus (specified)"), "{text}");
    let text = create_agent(&tool, Some("mini"))
        .await
        .expect("裸 id 应命中");
    assert!(text.contains("Model: mini (specified)"), "{text}");
}

#[tokio::test]
async fn unknown_model_error_lists_aliases_and_models() {
    let tool = create_tool(supervisor());
    let error = create_agent(&tool, Some("nope"))
        .await
        .expect_err("未知标识应报错");
    assert!(error.contains("unknown model or alias \"nope\""), "{error}");
    assert!(error.contains("smart"), "报错应列出别名：{error}");
    assert!(error.contains("anthropic/opus"), "{error}");
}

#[test]
fn description_lists_aliases_with_capability_tags_and_inheritance_note() {
    let tool = create_tool(supervisor());
    let desc = tool.definition().description;
    assert!(
        desc.contains("inherit the main agent's current model"),
        "{desc}"
    );
    assert!(desc.contains("smart → anthropic/opus"), "{desc}");
    assert!(desc.contains("[reasoning]"), "{desc}");
    assert!(desc.contains("[vision]"), "{desc}");
    assert!(desc.contains("- mini (mini, ctx 128k)"), "{desc}");
}
