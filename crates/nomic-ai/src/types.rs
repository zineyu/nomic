//! 统一消息模型：借鉴 pi-ai 的 `User` / `Assistant` / `ToolResult` 三角色模型，
//! 所有类型派生 serde，保证 session 持久化与 RPC 可直接落地。
//!
//! 与 pi 的差异见 `docs/adr/0001-pi-rust-architecture.md`。

use serde::{Deserialize, Serialize};

/// 推理/思考级别。`xhigh` 与 `max` 仅部分模型族支持。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// 最小推理预算
    Minimal,
    /// 低推理预算
    Low,
    /// 中等推理预算
    Medium,
    /// 高推理预算
    High,
    /// 超高推理预算（仅部分模型支持）
    Xhigh,
    /// 最大推理预算（仅部分模型支持）
    Max,
}

/// 文本内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextContent {
    /// 文本内容
    pub text: String,
    /// provider 侧的文本签名（如 OpenAI responses 的消息元数据），回放时原样带回
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

/// 推理/思考内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingContent {
    /// 思考文本
    pub thinking: String,
    /// Anthropic 思考签名，多轮续传必需，回放时原样带回
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// 是否被安全过滤器打码（打码时密文存于 `thinking_signature`）
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

/// 图片内容块（base64 内联）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    /// base64 编码的图片数据
    pub data: String,
    /// MIME 类型，如 `image/png`
    pub mime_type: String,
}

/// 工具调用内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// provider 分配的调用 id
    pub id: String,
    /// 工具名
    pub name: String,
    /// 调用参数（JSON 对象）
    pub arguments: serde_json::Value,
    /// Google 特有的思考签名，回放时原样带回
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// assistant 消息的内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    /// 文本
    Text(TextContent),
    /// 推理/思考
    Thinking(ThinkingContent),
    /// 工具调用
    ToolCall(ToolCall),
}

/// 用户/工具结果可携带的内容块。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    /// 文本
    Text(TextContent),
    /// 图片
    Image(ImageContent),
}

/// 用户消息内容：纯文本或内容块列表。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserMessageContent {
    /// 纯文本快捷形式
    Text(String),
    /// 内容块列表（可含图片）
    Blocks(Vec<UserContent>),
}

/// token 花费（美元）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    /// 输入 token 花费
    pub input: f64,
    /// 输出 token 花费
    pub output: f64,
    /// 缓存读取花费
    pub cache_read: f64,
    /// 缓存写入花费
    pub cache_write: f64,
    /// 总花费
    pub total: f64,
}

/// 单次 assistant 响应的 token 用量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// 输入 token 数
    pub input: u64,
    /// 输出 token 数（含 reasoning）
    pub output: u64,
    /// 缓存命中 token 数
    pub cache_read: u64,
    /// 缓存写入 token 数
    pub cache_write: u64,
    /// 推理 token 数（`output` 的子集，provider 报告时才有值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// 总 token 数
    pub total_tokens: u64,
    /// 按模型费率折算的花费
    #[serde(default)]
    pub cost: Cost,
}

/// assistant 响应的终止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// 正常结束
    Stop,
    /// 达到输出 token 上限（内容被截断）
    Length,
    /// 请求工具调用
    ToolUse,
    /// 运行时错误
    Error,
    /// 被中止
    Aborted,
}

/// 用户消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    /// 内容
    pub content: UserMessageContent,
    /// Unix 毫秒时间戳
    pub timestamp: u64,
}

/// assistant 消息（完整响应；流式过程中的增量见 [`crate::AssistantEvent`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// 有序内容块
    pub content: Vec<AssistantContent>,
    /// 发起请求的 API 种类
    pub api: ApiKind,
    /// provider 标识
    pub provider: String,
    /// 请求时使用的模型 id
    pub model: String,
    /// 响应中实际返回的模型 id（如 OpenRouter auto 路由），与请求不同时记录
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// provider 侧的响应 id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// token 用量
    pub usage: Usage,
    /// 终止原因
    pub stop_reason: StopReason,
    /// 错误信息（`stop_reason` 为 `Error` / `Aborted` 时存在）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Unix 毫秒时间戳
    pub timestamp: u64,
}

/// 工具执行结果消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultMessage {
    /// 对应的工具调用 id
    pub tool_call_id: String,
    /// 工具名
    pub tool_name: String,
    /// 结果内容（文本/图片）
    pub content: Vec<UserContent>,
    /// 结构化详情（日志与 UI 渲染用，不进 LLM 上下文）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// 是否为错误结果（同样回喂模型，由模型自我修正）
    pub is_error: bool,
    /// Unix 毫秒时间戳
    pub timestamp: u64,
}

/// 对话消息。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    /// 用户消息
    User(UserMessage),
    /// assistant 消息
    Assistant(AssistantMessage),
    /// 工具结果消息
    ToolResult(ToolResultMessage),
}

/// 工具的 JSON Schema 定义（发送给 provider）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// 工具名
    pub name: String,
    /// 工具描述（模型选择工具的主要依据）
    pub description: String,
    /// 参数的 JSON Schema
    pub parameters: serde_json::Value,
}

/// 一次 LLM 请求的完整上下文。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Context {
    /// 系统提示词
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// 消息历史
    pub messages: Vec<Message>,
    /// 可用工具
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
}

/// 支持的 API 种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKind {
    /// Anthropic Messages API
    AnthropicMessages,
    /// OpenAI Chat Completions API（含兼容端点）
    OpenAiCompletions,
}

/// 模型描述。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// 模型 id（发送给 API）
    pub id: String,
    /// 展示名
    pub name: String,
    /// 使用的 API
    pub api: ApiKind,
    /// provider 标识（如 `anthropic`、`openai`、`deepseek`）
    pub provider: String,
    /// API base URL
    pub base_url: String,
    /// 是否支持推理/思考
    #[serde(default)]
    pub reasoning: bool,
    /// 上下文窗口 token 数
    #[serde(default)]
    pub context_window: u64,
    /// 最大输出 token 数
    #[serde(default)]
    pub max_tokens: u64,
    /// 每百万 token 费率：输入
    #[serde(default)]
    pub cost_input: f64,
    /// 每百万 token 费率：输出
    pub cost_output: f64,
    /// 每百万 token 费率：缓存读取
    #[serde(default)]
    pub cost_cache_read: f64,
    /// 每百万 token 费率：缓存写入
    #[serde(default)]
    pub cost_cache_write: f64,
}

impl Model {
    /// 按模型费率计算一次响应的花费。
    // token 数量级远低于 f64 的 2^52 精确整数范围，精度损失可忽略
    #[allow(clippy::cast_precision_loss)]
    pub fn calculate_cost(&self, usage: &mut Usage) {
        let rate = |per_million: f64, tokens: u64| per_million * tokens as f64 / 1_000_000.0;
        usage.cost = Cost {
            input: rate(self.cost_input, usage.input),
            output: rate(self.cost_output, usage.output),
            cache_read: rate(self.cost_cache_read, usage.cache_read),
            cache_write: rate(self.cost_cache_write, usage.cache_write),
            total: 0.0,
        };
        usage.cost.total =
            usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
    }
}

/// 合成摘要消息的包装前缀（与 pi 的 `COMPACTION_SUMMARY_PREFIX` 一致）。
///
/// 该前缀同时是识别标记：session 重建、二次压缩提取 previous summary、
/// 交互端压缩渲染都靠它判定一条 user 消息是否为压缩摘要。
pub const SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n<summary>\n";

/// 合成摘要消息的包装后缀（与 pi 一致）。
pub const SUMMARY_SUFFIX: &str = "\n</summary>";

/// 构造压缩摘要的合成 user 消息（上下文压缩把较早消息段替换为该消息）。
pub fn summary_message(summary: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(format!("{SUMMARY_PREFIX}{summary}{SUMMARY_SUFFIX}")),
        timestamp,
    })
}

/// 判定一条消息是否为压缩摘要（包装前缀识别）。
pub fn is_summary_message(message: &Message) -> bool {
    extract_summary(message).is_some()
}

/// 从合成摘要消息中提取摘要正文（非摘要消息返回 `None`）。
pub fn extract_summary(message: &Message) -> Option<&str> {
    let Message::User(user) = message else {
        return None;
    };
    let UserMessageContent::Text(text) = &user.content else {
        return None;
    };
    text.strip_prefix(SUMMARY_PREFIX)
        .and_then(|rest| rest.strip_suffix(SUMMARY_SUFFIX))
}

/// 当前 Unix 毫秒时间戳。
pub fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Unix 毫秒时间在可预见的未来不会溢出 u64
    #[allow(clippy::cast_possible_truncation)]
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    millis
}
