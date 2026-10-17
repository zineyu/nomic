//! 执行环境抽象。
//!
//! 工具调用的派发目标。环境按会话绑定的 `working_dir` 解释路径
//! 坐标系；工具语义层面的成功、报错与拒绝经 [`ToolResult`] 三层
//! 返回，环境自身故障经 [`EnvironmentError`] 单独传播。

use std::future::Future;
use std::path::Path;

use crate::error::EnvironmentError;
use crate::id::SessionId;
use crate::tool::{ToolCall, ToolResult, ToolSpec};

/// 工具执行环境：按会话绑定的上下文派发工具调用。
pub trait ExecutionEnvironment: Send + Sync {
    /// 执行一次工具调用。
    ///
    /// `session` 为调用所属会话（派发层的关联标识），`working_dir`
    /// 为会话绑定的持久化工作区标识，相对路径的坐标系由本环境解释。
    ///
    /// 返回 `Ok(ToolResult)` 表达工具语义层面的三层结果——成功输出、
    /// 工具自身报错（[`crate::ToolResult::Error`]）、边界拒绝
    /// （[`crate::ToolResult::Rejected`]）；返回 `Err` 表示环境自身
    /// 故障（无法派发调用）。调用方必须区分两者：三层结果回灌模型，
    /// 故障中断当前步骤。
    fn execute(
        &self,
        session: SessionId,
        working_dir: &Path,
        call: &ToolCall,
    ) -> impl Future<Output = Result<ToolResult, EnvironmentError>> + Send;

    /// 当前可供派发的工具声明，随 LLM 请求传入供模型选择。
    fn tool_specs(&self) -> &[ToolSpec];
}
