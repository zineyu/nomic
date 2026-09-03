//! `create_agent` 工具：创建独立子 agent（supervisor 生命周期管理的入口）。
//!
//! 模型解析语义（继承主 agent 模型 / 别名 / 全形式 / 裸 id）与工具描述
//! 的别名、能力标签展示收在本模块；落库接缝 [`ChildSessionHook`] 在
//! create 成功后接管子 agent 事件流（ADR-0044）。

use std::sync::Arc;

use async_trait::async_trait;
use nomic_core::{
    AgentSupervisor, AgentTool, CreateAgentRequest, DynTool, ExecutionMode, ToolError, ToolResult,
    ToolUpdateCallback,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{ChildSessionHook, filter_tools};

/// 模型的能力标签（写入工具描述，供 LLM 按任务需求区分模型）：
/// `reasoning` = 推理/思考（智力维度），`vision` = 图像输入（多模态维度）。
fn capability_tags(m: &nomic_ai::Model) -> String {
    let mut tags = Vec::new();
    if m.reasoning {
        tags.push("reasoning");
    }
    if m.vision {
        tags.push("vision");
    }
    if tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", tags.join("] ["))
    }
}

/// 单行模型描述：`- <id> (<名称><能力标签>, ctx Nk)`。
fn model_line(m: &nomic_ai::Model) -> String {
    format!(
        "- {} ({}{}, ctx {}k)",
        m.id,
        m.name,
        capability_tags(m),
        m.context_window / 1000
    )
}

/// 根据可用模型列表生成模型描述文本（写入工具 description）。
fn models_description(available_models: &[nomic_ai::Model]) -> String {
    if available_models.is_empty() {
        return String::from("(no models available)");
    }
    available_models
        .iter()
        .map(model_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 根据别名表生成别名描述文本（写入工具 description）：每个别名带目标
/// 模型与能力标签，LLM 据此按智力 / 多模态需求选择别名。
fn aliases_description(aliases: &std::collections::BTreeMap<String, nomic_ai::Model>) -> String {
    if aliases.is_empty() {
        return String::from("(no aliases configured)");
    }
    aliases
        .iter()
        .map(|(alias, m)| {
            format!(
                "- {alias} → {}/{}",
                m.provider,
                model_line(m).trim_start_matches("- ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `create_agent` 工具参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAgentParams {
    /// Agent 的唯一标识（可选；不提供则自动生成 UUID）。
    pub id: Option<String>,
    /// 使用的模型（可选）：模型别名或模型 ID / `<provider>/<id>`（从下方
    /// 别名表与可用模型列表中选择）；不提供时继承主 agent 的当前模型。
    #[serde(default)]
    pub model: Option<String>,
    /// 系统提示词（必填；定义该 agent 的角色和行为）。
    pub system_prompt: String,
    /// 该 agent 可以使用的工具名称列表（子集）。
    #[serde(default)]
    pub tool_names: Vec<String>,
}

/// `create_agent` 工具：创建独立子 agent。
pub struct CreateAgentTool {
    supervisor: Arc<AgentSupervisor>,
    available_tools: Vec<DynTool>,
    /// 子 agent 事件流接缝（落库用，见类型文档）；`None` = 不落库
    child_hook: Option<ChildSessionHook>,
    description: String,
}

impl std::fmt::Debug for CreateAgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateAgentTool").finish_non_exhaustive()
    }
}

impl CreateAgentTool {
    /// 创建工具实例。
    ///
    /// - `supervisor`：共享的 supervisor。
    /// - `available_tools`：可供子 agent 分配的工具池。
    /// - `child_hook`：子 agent 事件流接缝（落库用）；`None` 时子 agent
    ///   对话不落库。
    pub fn new(
        supervisor: Arc<AgentSupervisor>,
        available_tools: Vec<DynTool>,
        child_hook: Option<ChildSessionHook>,
    ) -> Self {
        let aliases_desc = aliases_description(supervisor.aliases());
        let models_desc = models_description(supervisor.available_models());
        let tool_names: Vec<&str> = available_tools.iter().map(DynTool::name).collect();
        let description = format!(
            "Create an independent child agent with its own system prompt, tools, and model. \
             Returns the agent ID for use with send_message / wait_result / close_agent.\n\n\
             The `model` param is optional: pass a model alias or a model ID; omit it to let \
             the child inherit the main agent's current model (the default). Aliases are \
             user-configured shortcuts tagged by capability ([reasoning] = intelligence, \
             [vision] = multimodal image input) — prefer them when the task has clear \
             capability needs.\n\n\
             Model aliases:\n{aliases_desc}\n\n\
             Available models:\n{models_desc}\n\n\
             Available tools for assignment:\n{}",
            tool_names.join(", ")
        );
        Self {
            supervisor,
            available_tools,
            child_hook,
            description,
        }
    }
}

#[allow(clippy::unnecessary_literal_bound)]
#[async_trait]
impl AgentTool for CreateAgentTool {
    type Params = CreateAgentParams;

    fn name(&self) -> &'static str {
        "create_agent"
    }

    fn label(&self) -> &str {
        "创建子 Agent"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    async fn execute(
        &self,
        params: Self::Params,
        _cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        // 模型三层解析：缺省继承主 agent 当前模型 > 别名 > 模型 ID / 全形式
        let (model, source) = match params.model.as_deref() {
            None => (
                self.supervisor.inherited_model(),
                "inherited from main agent".to_string(),
            ),
            Some(spec) => {
                let model = self.supervisor.resolve_model(spec).map_err(|e| {
                    tracing::warn!(spec, error = %e, "unknown model for child agent");
                    ToolError::new(e.to_string())
                })?;
                let source = if self.supervisor.aliases().contains_key(spec.trim()) {
                    format!("alias \"{}\"", spec.trim())
                } else {
                    "specified".to_string()
                };
                (model, source)
            }
        };

        let tools = filter_tools(&self.available_tools, &params.tool_names);

        tracing::info!(
            model = %model.id,
            source = %source,
            tool_count = tools.len(),
            id = ?params.id,
            "creating child agent"
        );
        let id = self
            .supervisor
            .create(CreateAgentRequest {
                id: params.id,
                system_prompt: params.system_prompt,
                tools,
                model: model.clone(),
                provider: None,
                stream_options: None,
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to create child agent");
                ToolError::new(e.to_string())
            })?;

        // 落库接缝：接管事件流（hook 内创建同 work 的子 session 并 spawn
        // 落库任务）；无 hook 时事件流留在 supervisor 随 close 丢弃
        if let Some(hook) = &self.child_hook
            && let Some(events) = self.supervisor.take_events(&id).await
        {
            hook(id.clone(), events);
        }

        tracing::info!(agent_id = %id, model = %model.id, "child agent created");
        Ok(ToolResult::text(format!(
            "Agent created successfully.\n  ID: {id}\n  Model: {} ({source})\n  Tools: [{}]",
            model.id,
            if params.tool_names.is_empty() {
                "none".to_string()
            } else {
                params.tool_names.join(", ")
            }
        )))
    }
}
