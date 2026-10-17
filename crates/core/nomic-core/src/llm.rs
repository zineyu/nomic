//! LLM 适配器抽象。
//!
//! nomic-ai crate 以 rig-core 实现本 trait；这是 rig 类型
//! 唯一允许出现的位置。测试中用脚本化替身按剧本返回
//! 流式 token 与工具调用请求。

use std::future::Future;
use std::pin::Pin;

use futures_core::Stream;

use crate::error::LlmError;
use crate::id::SessionId;
use crate::message::Message;
use crate::tool::{ToolCall, ToolSpec};

/// 适配器产出的事件流元素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmEvent {
    /// 流式文本增量。
    Token(String),
    /// 模型请求执行工具。
    ToolCallRequested(ToolCall),
}

/// 适配器事件流：异步、元素为 [`LlmEvent`] 或流中途的 [`LlmError`]。
///
/// 流一旦产出错误即终止；错误是失败事实，不是事件，调用方必须
/// 显式处理或向上传播。
pub type LlmEventStream = Pin<Box<dyn Stream<Item = Result<LlmEvent, LlmError>> + Send>>;

/// LLM 适配器：把 provider 协议收敛为 nomic 领域类型。
pub trait LlmAdapter: Send + Sync {
    /// 请求模型继续会话：传入完整消息历史与可用工具声明，
    /// 返回流式事件。建立请求即失败时直接返回错误；已经开始的
    /// 流中途失败时以流元素的错误项给出。重试策略由调用方
    /// （server）施加。
    ///
    /// `messages` 以系统提示开头，回放与恢复路径必须产生
    /// 与原始执行一致的序列。
    fn respond(
        &self,
        session: SessionId,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> impl Future<Output = Result<LlmEventStream, LlmError>> + Send;
}
