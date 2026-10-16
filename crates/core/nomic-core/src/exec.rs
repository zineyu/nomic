//! 执行环境抽象。
//!
//! 工具调用的派发目标。边界校验（工作目录 lexical 校验 +
//! 符号链接解析）在各执行环境内部统一执行：越界不返回错误，
//! 而返回 [`ToolResult::Rejected`]，拒绝原因对用户可见。

use std::future::Future;

use crate::error::EnvironmentError;
use crate::id::SessionId;
use crate::tool::{ToolCall, ToolResult};

/// 工具执行环境：按会话绑定的上下文派发工具调用。
pub trait ExecutionEnvironment: Send + Sync {
    /// 执行一次工具调用。
    ///
    /// 返回 `Ok(ToolResult)` 表达工具语义层面的三层结果——成功输出、
    /// 工具自身报错（[`crate::ToolResult::Error`]）、边界拒绝
    /// （[`crate::ToolResult::Rejected`]）；返回 `Err` 表示环境自身
    /// 故障（无法派发调用）。调用方必须区分两者：三层结果回灌模型，
    /// 故障中断当前步骤。
    fn execute(
        &self,
        session: SessionId,
        call: &ToolCall,
    ) -> impl Future<Output = Result<ToolResult, EnvironmentError>> + Send;
}
