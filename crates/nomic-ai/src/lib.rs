//! nomic-ai：统一的多 provider LLM 流式抽象（对应 pi 的 pi-ai 层）。
//!
//! 核心内容：
//! - `types`：统一消息模型（`Message` / 内容块 / `Usage` / `StopReason`）
//! - `compaction`：压缩摘要消息的构造与识别、有效上下文重建语义
//! - `stream`：流式事件协议与 [`Provider`] 契约（错误编码进流，不抛出）
//! - [`providers`]：Anthropic Messages 与 `OpenAI` Completions 兼容实现
//! - [`models_dev`]：models.dev 模型目录（按模型 id 查询规格，磁盘缓存）
//!
//! 设计决策见仓库 `docs/adr/0001-pi-rust-architecture.md`。

pub mod compaction;
pub mod models_dev;
pub mod providers;
mod stream;
mod types;

pub use compaction::{
    SUMMARY_PREFIX, SUMMARY_SUFFIX, apply_compaction, extract_summary, is_summary_message,
    summary_message,
};
pub use models_dev::{Catalog, ModelSpec};
pub use stream::{AssistantEvent, AssistantStream, Provider, StreamOptions, channel};
pub use types::{
    ApiKind, AssistantContent, AssistantMessage, Context, Cost, ImageContent, Message, Model,
    StopReason, TextContent, ThinkingContent, ThinkingLevel, ToolCall, ToolDefinition,
    ToolResultMessage, Usage, UserContent, UserMessage, UserMessageContent, now_millis,
};
