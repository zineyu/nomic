//! Agent loop：worker 内执行引擎。
//!
//! 单轮对话形态：领取待处理会话 → 组装系统提示与历史 →
//! 请求 LLM（流式）→ 助手消息落库 → 释放会话回静止。
//! 工具调用回合在后续切片落地。

mod prompt;
mod worker;

pub use prompt::{DEFAULT_SYSTEM_PROMPT, assemble_system_prompt};
pub use worker::{AgentError, Worker};
