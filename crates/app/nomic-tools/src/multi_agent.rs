//! 多 agent 管理工具：让主 agent 通过工具调用来创建、通信和管理子 agent。
//!
//! 六个工具对应 supervisor 的核心操作：
//!
//! | 工具名 | 阻塞性 | 说明 |
//! |--------|--------|------|
//! | `create_agent` | 否 | 创建子 agent（指定模型、系统提示词、工具子集） |
//! | `send_message` | **否** | 向子 agent 发送消息，立即返回 |
//! | `wait_result` | **是** | 等待子 agent 完成，返回 assistant 回复 |
//! | `wait_all` | **是** | 等待多个子 agent 全部完成 |
//! | `close_agent` | 否 | 关闭子 agent，释放资源 |
//! | `list_agents` | 否 | 列出所有子 agent 及其状态 |
//!
//! ## fork-join 典型流程
//!
//! ```text
//! create_agent(id="a", model="claude-sonnet-4-20250514", system_prompt="...", tool_names=[...])
//! create_agent(id="b", model="gpt-4o", system_prompt="...", tool_names=[...])
//! send_message(agent_id="a", message="任务 A")   ← 非阻塞
//! send_message(agent_id="b", message="任务 B")   ← 非阻塞
//! wait_all(agent_ids=["a","b"])                    ← 阻塞等待全部
//! close_agent(agent_id="a")
//! close_agent(agent_id="b")
//! ```
//!
//! ## 模型选择
//!
//! `create_agent` 的 `model` 参数为必填项。可用模型列表在工具构造时注入，
//! 写入工具描述供 LLM 参照选择，同时用于运行时校验。

use std::sync::Arc;

use async_trait::async_trait;
use nomic_ai::Message;
use nomic_core::{
    AgentId, AgentSupervisor, AgentTool, CreateAgentRequest, DynTool, ExecutionMode, ToolError,
    ToolResult, ToolUpdateCallback,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

// ── 辅助 ──────────────────────────────────────────────────────────────

/// 从 `available_tools` 中按名称筛选出 `names` 指定的工具子集。
fn filter_tools(available_tools: &[DynTool], names: &[String]) -> Vec<DynTool> {
    available_tools
        .iter()
        .filter(|t| names.iter().any(|n| n == t.name()))
        .cloned()
        .collect()
}

/// 格式化 agent 回复消息为可读文本（提取 assistant 消息的文本内容）。
fn format_messages(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    for msg in messages {
        match msg {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let nomic_ai::AssistantContent::Text(text) = block {
                        parts.push(text.text.clone());
                    }
                }
            }
            Message::ToolResult(result) => {
                for block in &result.content {
                    if let nomic_ai::UserContent::Text(text) = block {
                        parts.push(format!("[tool:{}]: {}", result.tool_name, text.text));
                    }
                }
            }
            Message::User(_) => {}
        }
    }
    parts.join("\n")
}

/// 根据可用模型列表生成模型描述文本（写入工具 description）。
fn models_description(available_models: &[nomic_ai::Model]) -> String {
    if available_models.is_empty() {
        return String::from("(no models available)");
    }
    let mut lines = Vec::new();
    for m in available_models {
        let reasoning_tag = if m.reasoning { " [reasoning]" } else { "" };
        lines.push(format!(
            "- {} ({}{}, ctx {}k)",
            m.id,
            m.name,
            reasoning_tag,
            m.context_window / 1000
        ));
    }
    lines.join("\n")
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tool 1: create_agent
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// `create_agent` 工具参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateAgentParams {
    /// Agent 的唯一标识（可选；不提供则自动生成 UUID）。
    pub id: Option<String>,
    /// 使用的模型 ID（必填；从下方可用模型列表中选择）。
    pub model: String,
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
    pub fn new(supervisor: Arc<AgentSupervisor>, available_tools: Vec<DynTool>) -> Self {
        let models_desc = models_description(supervisor.available_models());
        let tool_names: Vec<&str> = available_tools.iter().map(DynTool::name).collect();
        let description = format!(
            "Create an independent child agent with its own system prompt, tools, and model. \
             Returns the agent ID for use with send_message / wait_result / close_agent.\n\n\
             Available models:\n{models_desc}\n\n\
             Available tools for assignment:\n{}",
            tool_names.join(", ")
        );
        Self {
            supervisor,
            available_tools,
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
        let model = self
            .supervisor
            .available_models()
            .iter()
            .find(|m| m.id == params.model)
            .cloned()
            .ok_or_else(|| {
                let available: Vec<&str> = self
                    .supervisor
                    .available_models()
                    .iter()
                    .map(|m| m.id.as_str())
                    .collect();
                ToolError::new(format!(
                    "unknown model \"{}\"; available: [{}]",
                    params.model,
                    available.join(", ")
                ))
            })?;

        let tools = filter_tools(&self.available_tools, &params.tool_names);

        let id = self
            .supervisor
            .create(CreateAgentRequest {
                id: params.id,
                system_prompt: params.system_prompt,
                tools,
                model,
                provider: None,
                stream_options: None,
            })
            .await
            .map_err(|e| ToolError::new(e.to_string()))?;

        Ok(ToolResult::text(format!(
            "Agent created successfully.\n  ID: {id}\n  Model: {}\n  Tools: [{}]",
            params.model,
            if params.tool_names.is_empty() {
                "none".to_string()
            } else {
                params.tool_names.join(", ")
            }
        )))
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tool 2: send_message（非阻塞）
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// `send_message` 工具参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendMessageParams {
    /// 目标 agent ID（由 `create_agent` 返回）。
    pub agent_id: String,
    /// 要发送的消息文本。
    pub message: String,
}

/// `send_message` 工具（非阻塞）。
pub struct SendMessageTool {
    supervisor: Arc<AgentSupervisor>,
}

impl std::fmt::Debug for SendMessageTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendMessageTool").finish_non_exhaustive()
    }
}

impl SendMessageTool {
    /// 创建工具实例。
    pub const fn new(supervisor: Arc<AgentSupervisor>) -> Self {
        Self { supervisor }
    }
}

#[allow(clippy::unnecessary_literal_bound)]
#[async_trait]
impl AgentTool for SendMessageTool {
    type Params = SendMessageParams;

    fn name(&self) -> &'static str {
        "send_message"
    }

    fn label(&self) -> &str {
        "发送消息给子 Agent"
    }

    fn description(&self) -> &str {
        "Send a message to a child agent. This is NON-BLOCKING: it returns \
         immediately while the agent processes the message in the background. \
         Use wait_result or wait_all to get the agent's response."
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    async fn execute(
        &self,
        params: Self::Params,
        cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        let id = AgentId(params.agent_id.clone());
        self.supervisor
            .send_message(&id, &params.message, cancel)
            .await
            .map_err(|e| ToolError::new(e.to_string()))?;

        Ok(ToolResult::text(format!(
            "Message sent to agent \"{}\" (non-blocking). Use wait_result to get the response.",
            params.agent_id
        )))
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tool 3: wait_result（阻塞）
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// `wait_result` 工具参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitResultParams {
    /// 目标 agent ID。
    pub agent_id: String,
}

/// `wait_result` 工具（阻塞）。
pub struct WaitResultTool {
    supervisor: Arc<AgentSupervisor>,
}

impl std::fmt::Debug for WaitResultTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitResultTool").finish_non_exhaustive()
    }
}

impl WaitResultTool {
    /// 创建工具实例。
    pub const fn new(supervisor: Arc<AgentSupervisor>) -> Self {
        Self { supervisor }
    }
}

#[allow(clippy::unnecessary_literal_bound)]
#[async_trait]
impl AgentTool for WaitResultTool {
    type Params = WaitResultParams;

    fn name(&self) -> &'static str {
        "wait_result"
    }

    fn label(&self) -> &str {
        "等待子 Agent 结果"
    }

    fn description(&self) -> &str {
        "Wait for a child agent to finish processing and return its response. \
         This is BLOCKING: it waits until the agent completes its current task. \
         Use this after send_message to collect the agent's response."
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    async fn execute(
        &self,
        params: Self::Params,
        _cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        let id = AgentId(params.agent_id.clone());
        let messages = self
            .supervisor
            .wait_result(&id)
            .await
            .map_err(|e| ToolError::new(e.to_string()))?;

        let text = format_messages(&messages);
        if text.is_empty() {
            Ok(ToolResult::text(format!(
                "Agent \"{}\" completed with no text response.",
                params.agent_id
            )))
        } else {
            Ok(ToolResult::text(format!(
                "Agent \"{}\" response:\n{}",
                params.agent_id, text
            )))
        }
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tool 4: wait_all（阻塞）
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// `wait_all` 工具参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct WaitAllParams {
    /// 要等待的 agent ID 列表。
    pub agent_ids: Vec<String>,
}

/// `wait_all` 工具（阻塞）：并发等待多个子 agent 全部完成。
pub struct WaitAllTool {
    supervisor: Arc<AgentSupervisor>,
}

impl std::fmt::Debug for WaitAllTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitAllTool").finish_non_exhaustive()
    }
}

impl WaitAllTool {
    /// 创建工具实例。
    pub const fn new(supervisor: Arc<AgentSupervisor>) -> Self {
        Self { supervisor }
    }
}

#[allow(clippy::unnecessary_literal_bound)]
#[async_trait]
impl AgentTool for WaitAllTool {
    type Params = WaitAllParams;

    fn name(&self) -> &'static str {
        "wait_all"
    }

    fn label(&self) -> &str {
        "等待所有子 Agent"
    }

    fn description(&self) -> &str {
        "Wait for multiple child agents to ALL finish and return their responses. \
         This is BLOCKING. All agents are awaited concurrently (total time = slowest agent). \
         Use this after sending messages to multiple agents for fork-join patterns."
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    async fn execute(
        &self,
        params: Self::Params,
        _cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        use std::fmt::Write as _;
        let ids: Vec<AgentId> = params
            .agent_ids
            .iter()
            .map(|s| AgentId(s.clone()))
            .collect();
        let results = self
            .supervisor
            .wait_all(&ids)
            .await
            .map_err(|e| ToolError::new(e.to_string()))?;

        let mut output = String::new();
        for id_str in &params.agent_ids {
            let id = AgentId(id_str.clone());
            if let Some(messages) = results.get(&id) {
                let text = format_messages(messages);
                let _ = write!(
                    output,
                    "=== Agent \"{}\" ===\n{}\n\n",
                    id_str,
                    if text.is_empty() {
                        "(no text response)"
                    } else {
                        &text
                    }
                );
            }
        }

        Ok(ToolResult::text(format!(
            "All {} agents completed.\n\n{}",
            params.agent_ids.len(),
            output
        )))
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tool 5: close_agent
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// `close_agent` 工具参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CloseAgentParams {
    /// 要关闭的 agent ID。
    pub agent_id: String,
}

/// `close_agent` 工具：关闭子 agent 并释放资源。
pub struct CloseAgentTool {
    supervisor: Arc<AgentSupervisor>,
}

impl std::fmt::Debug for CloseAgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloseAgentTool").finish_non_exhaustive()
    }
}

impl CloseAgentTool {
    /// 创建工具实例。
    pub const fn new(supervisor: Arc<AgentSupervisor>) -> Self {
        Self { supervisor }
    }
}

#[allow(clippy::unnecessary_literal_bound)]
#[async_trait]
impl AgentTool for CloseAgentTool {
    type Params = CloseAgentParams;

    fn name(&self) -> &'static str {
        "close_agent"
    }

    fn label(&self) -> &str {
        "关闭子 Agent"
    }

    fn description(&self) -> &str {
        "Close a child agent and release its resources. The agent ID becomes \
         invalid after this call. Always close agents when done to free resources."
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
        let id = AgentId(params.agent_id.clone());
        self.supervisor
            .close(&id)
            .await
            .map_err(|e| ToolError::new(e.to_string()))?;

        Ok(ToolResult::text(format!(
            "Agent \"{}\" closed successfully.",
            params.agent_id
        )))
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tool 6: list_agents
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// `list_agents` 的参数类型（无字段）。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListAgentsParams {}

/// `list_agents` 工具：列出所有子 agent 及其状态。
pub struct ListAgentsTool {
    supervisor: Arc<AgentSupervisor>,
}

impl std::fmt::Debug for ListAgentsTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListAgentsTool").finish_non_exhaustive()
    }
}

impl ListAgentsTool {
    /// 创建工具实例。
    pub const fn new(supervisor: Arc<AgentSupervisor>) -> Self {
        Self { supervisor }
    }
}

#[allow(clippy::unnecessary_literal_bound)]
#[async_trait]
impl AgentTool for ListAgentsTool {
    type Params = ListAgentsParams;

    fn name(&self) -> &'static str {
        "list_agents"
    }

    fn label(&self) -> &str {
        "列出子 Agent"
    }

    fn description(&self) -> &str {
        "List all child agents with their status (running/idle), model, message count, \
         and system prompt preview. Use this to check the state of your agents."
    }

    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    async fn execute(
        &self,
        _params: Self::Params,
        _cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        let statuses = self.supervisor.list().await;

        if statuses.is_empty() {
            return Ok(ToolResult::text("No child agents."));
        }

        let mut lines = Vec::new();
        for s in &statuses {
            let state = if s.is_running { "RUNNING" } else { "idle" };
            lines.push(format!(
                "- {} [{}] | model={} | msgs={} | prompt=\"{}\"",
                s.id, state, s.model_id, s.message_count, s.system_prompt_preview
            ));
        }

        Ok(ToolResult::text(format!(
            "Child agents ({}):\n{}",
            statuses.len(),
            lines.join("\n")
        )))
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// 工具集构造
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 创建多 agent 管理工具集（6 个工具）。
///
/// - `supervisor`：共享的 supervisor 实例。
/// - `available_tools`：可供子 agent 分配的工具池（不含管理工具本身，
///   避免子 agent 递归创建子 agent）。
///
/// 返回的工具列表可直接传入主 agent 的 builder `.tools()`。
pub fn multi_agent_tools(
    supervisor: Arc<AgentSupervisor>,
    available_tools: Vec<DynTool>,
) -> Vec<DynTool> {
    vec![
        DynTool::new(CreateAgentTool::new(supervisor.clone(), available_tools)),
        DynTool::new(SendMessageTool::new(supervisor.clone())),
        DynTool::new(WaitResultTool::new(supervisor.clone())),
        DynTool::new(WaitAllTool::new(supervisor.clone())),
        DynTool::new(CloseAgentTool::new(supervisor.clone())),
        DynTool::new(ListAgentsTool::new(supervisor)),
    ]
}
