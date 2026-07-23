//! nomic-ai：统一的多 provider LLM 流式抽象（对应 pi 的 pi-ai 层）。
//!
//! 核心内容：
//! - `types`：统一消息模型（`Message` / 内容块 / `Usage` / `StopReason`）
//! - `stream`：流式事件协议与 [`Provider`] 契约（错误编码进流，不抛出）
//! - [`providers`]：Anthropic Messages 与 `OpenAI` Completions 兼容实现
//!
//! 设计决策见仓库 `docs/adr/0001-pi-rust-architecture.md`。

pub mod providers;
mod stream;
mod types;

pub use stream::{AssistantEvent, AssistantStream, Provider, StreamOptions, channel};
pub use types::{
    ApiKind, AssistantContent, AssistantMessage, Context, Cost, ImageContent, Message, Model,
    StopReason, TextContent, ThinkingContent, ThinkingLevel, ToolCall, ToolDefinition,
    ToolResultMessage, Usage, UserContent, UserMessage, UserMessageContent, now_millis,
};
