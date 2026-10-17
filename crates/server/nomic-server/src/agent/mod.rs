//! Agent loop：worker 内执行引擎。
//!
//! 领取待处理会话 → 组装系统提示与历史 → 请求 LLM（流式）→
//! 工具调用回合（执行环境派发、结果回灌）→ 释放会话回静止。
//! 历史回放的显式构造在 history 模块。

mod history;
mod prompt;
mod worker;

pub use history::{PendingCall, partial_messages, pending_call, replay_messages};
pub use prompt::{DEFAULT_SYSTEM_PROMPT, assemble_system_prompt};
pub use worker::{AgentError, AgentRunError, RunError, Worker};
