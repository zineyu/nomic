//! 工具抽象：[`AgentTool`]（强类型参数）与 [`DynTool`]（类型擦除包装）。
//!
//! 借鉴 pi 的设计：
//! - 工具执行失败**不抛出给 loop**，而是转为 `is_error = true` 的
//!   [`ToolResultMessage`] 回喂模型，由模型自我修正；
//! - 参数校验即 schemars + serde 反序列化（替代 pi 的 typebox 运行时校验）；
//! - 工具可通过 [`ExecutionMode::Sequential`] 声明必须串行执行。

use std::fmt;

use async_trait::async_trait;
use nomic_ai::{ToolDefinition, UserContent};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;

/// 工具执行错误。转为错误工具结果回喂模型。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ToolError {
    message: String,
}

impl ToolError {
    /// 创建错误。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

/// 工具执行模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// 可与同批次其他工具调用并发执行（默认）
    #[default]
    Parallel,
    /// 必须与同批次其他工具调用串行执行
    Sequential,
}

/// 工具执行的最终结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// 返回给模型的内容（文本/图片）
    pub content: Vec<UserContent>,
    /// 结构化详情（日志与 UI 渲染用，不进 LLM 上下文）
    pub details: Option<serde_json::Value>,
    /// 提前终止提示：当本批次所有结果都设置时，agent 在本批次后停止
    pub terminate: bool,
}

impl ToolResult {
    /// 纯文本结果。
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![UserContent::Text(nomic_ai::TextContent {
                text: text.into(),
                text_signature: None,
            })],
            details: None,
            terminate: false,
        }
    }
}

/// 工具执行中的进度更新。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUpdate {
    /// 当前已产生的部分内容
    pub content: Vec<UserContent>,
    /// 结构化详情
    pub details: Option<serde_json::Value>,
}

/// 进度更新回调（仅本次 `execute` 调用期间有效）。
pub type ToolUpdateCallback = Box<dyn Fn(ToolUpdate) + Send>;

/// 强类型工具契约。参数类型即校验：反序列化失败会作为错误结果回喂模型。
#[async_trait]
pub trait AgentTool: Send + Sync + 'static {
    /// 参数类型；JSON Schema 自动派生
    type Params: DeserializeOwned + JsonSchema + Send;

    /// 工具名（模型调用时使用的标识）
    fn name(&self) -> &'static str;
    /// 展示名（UI 用）
    fn label(&self) -> &str;
    /// 工具描述（模型选择工具的主要依据）
    fn description(&self) -> &str;
    /// 执行模式（默认并发）
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    /// 执行工具。返回 `Err` 会转为错误工具结果回喂模型。
    async fn execute(
        &self,
        params: Self::Params,
        cancel: CancellationToken,
        on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError>;
}

/// 类型擦除后的工具（loop 实际操作的对象）。
#[derive(Clone)]
pub struct DynTool {
    inner: std::sync::Arc<dyn ErasedTool>,
}

impl fmt::Debug for DynTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynTool")
            .field("name", &self.name())
            .finish_non_exhaustive()
    }
}

#[async_trait]
trait ErasedTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn label(&self) -> &str;
    fn execution_mode(&self) -> ExecutionMode;
    fn definition(&self) -> ToolDefinition;
    async fn execute_erased(
        &self,
        args: serde_json::Value,
        cancel: CancellationToken,
        on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError>;
}

/// [`AgentTool`] 到 [`ErasedTool`] 的适配器；构造时一次性生成 JSON Schema。
struct ToolAdapter<T: AgentTool> {
    tool: T,
    definition: ToolDefinition,
}

impl<T: AgentTool> ToolAdapter<T> {
    fn new(tool: T) -> Self {
        let schema = schemars::schema_for!(T::Params);
        let definition = ToolDefinition {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            parameters: serde_json::to_value(schema)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new())),
        };
        Self { tool, definition }
    }
}

#[async_trait]
impl<T: AgentTool> ErasedTool for ToolAdapter<T> {
    fn name(&self) -> &'static str {
        self.tool.name()
    }

    fn label(&self) -> &str {
        self.tool.label()
    }

    fn execution_mode(&self) -> ExecutionMode {
        self.tool.execution_mode()
    }

    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    async fn execute_erased(
        &self,
        args: serde_json::Value,
        cancel: CancellationToken,
        on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        // 反序列化即校验；错误文本回喂模型供其修正参数
        let params: T::Params = serde_json::from_value(args).map_err(|e| {
            ToolError::new(format!(
                "invalid arguments for tool `{}`: {e}",
                self.tool.name()
            ))
        })?;
        self.tool.execute(params, cancel, on_update).await
    }
}

impl DynTool {
    /// 包装一个强类型工具。
    pub fn new<T: AgentTool>(tool: T) -> Self {
        Self {
            inner: std::sync::Arc::new(ToolAdapter::new(tool)),
        }
    }

    /// 工具名。
    pub fn name(&self) -> &'static str {
        self.inner.name()
    }

    /// 展示名。
    pub fn label(&self) -> &str {
        self.inner.label()
    }

    /// 执行模式。
    pub fn execution_mode(&self) -> ExecutionMode {
        self.inner.execution_mode()
    }

    /// 发送给 provider 的工具定义。
    pub fn definition(&self) -> ToolDefinition {
        self.inner.definition()
    }

    /// 以未校验的 JSON 参数执行。
    pub async fn execute(
        &self,
        args: serde_json::Value,
        cancel: CancellationToken,
        on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        self.inner.execute_erased(args, cancel, on_update).await
    }
}
