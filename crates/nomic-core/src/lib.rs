//! nomic-core：agent loop 与工具抽象（对应 pi 的 pi-agent-core 层）。
//!
//! - `tool`：[`AgentTool`] 强类型工具契约与 [`DynTool`] 类型擦除包装
//! - `hooks`：生命周期 hooks（trait + 默认空实现）
//! - `agent`：事件驱动的 agent loop

mod agent;
mod compaction;
mod hooks;
mod tool;

pub use agent::{Agent, AgentConfig, AgentError, AgentEvent};
pub use compaction::{
    CompactRequest, Compaction, CompactionError, CompactionSettings, estimate_context_tokens,
    is_summary_message, should_compact, summary_message,
};
pub use hooks::{
    AfterToolCall, AfterToolCallOverride, AgentHooks, BeforeToolCall, NoopHooks, ToolCallDecision,
};
pub use tool::{
    AgentTool, DynTool, ExecutionMode, ToolError, ToolResult, ToolUpdate, ToolUpdateCallback,
};
