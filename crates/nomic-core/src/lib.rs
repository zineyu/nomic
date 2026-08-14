//! nomic-core：agent loop 与工具抽象（对应 pi 的 pi-agent-core 层）。
//!
//! - `tool`：[`AgentTool`] 强类型工具契约与 [`DynTool`] 类型擦除包装
//! - `hooks`：生命周期 hooks（trait + 默认空实现）
//! - `agent`：事件驱动的 agent loop（actor 封装见 [`AgentHandle`]，ADR-0022）
//! - `injection`：运行中注入点（turn 边界转向，交互端实现注入源）
//! - `builder`：typestate 创建 builder（编译期强制必填项）

mod agent;
mod builder;
mod compaction;
mod hooks;
mod injection;
mod tool;

pub use agent::{ActorError, Agent, AgentError, AgentEvent, AgentHandle};
pub use builder::{AgentBuilder, Set, Unset};
pub use compaction::{
    CompactRequest, Compaction, CompactionError, CompactionSettings, estimate_context_tokens,
    is_summary_message, should_compact, summary_message, usage_context_tokens,
};
pub use hooks::{
    AfterToolCall, AfterToolCallOverride, AgentHooks, BeforeToolCall, NoopHooks, ToolCallDecision,
};
pub use injection::{TurnInjection, TurnMessage};
pub use tool::{
    AgentTool, DynTool, ExecutionMode, ToolError, ToolResult, ToolUpdate, ToolUpdateCallback,
};
