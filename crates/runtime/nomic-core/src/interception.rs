//! agent 事件拦截（event interception）：把原先分散的 hooks 并入事件流（ADR-0028）。
//!
//! 拦截点与事件词汇对齐：loop 直接 `await` 拦截器做出门控 / 改写决策，
//! 观察者仍经 [`crate::AgentEvent`] 单向广播——「干预」与「观察」分属两套通道。
//!
//! M1 只提供工具执行前后两个拦截点（权限门控、结果改写的位置）；其余
//! 生命周期事件当前只有观察需求，不设拦截点。

use async_trait::async_trait;
use nomic_ai::UserContent;

use crate::tool::ToolResult;

/// `on_tool_execution_start` 的决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallDecision {
    /// 放行执行
    Allow,
    /// 阻止执行；`reason` 作为错误工具结果回喂模型
    Block {
        /// 阻止原因
        reason: String,
    },
}

/// `on_tool_execution_start` 拦截点（与 [`crate::AgentEvent::ToolExecutionStart`] 同字段）。
#[derive(Debug)]
pub struct ToolExecutionStart<'a> {
    /// 工具调用 id
    pub tool_call_id: &'a str,
    /// 工具名
    pub tool_name: &'a str,
    /// 原始参数
    pub args: &'a serde_json::Value,
}

/// `on_tool_execution_end` 拦截点（与 [`crate::AgentEvent::ToolExecutionEnd`] 同字段）。
#[derive(Debug)]
pub struct ToolExecutionEnd<'a> {
    /// 工具调用 id
    pub tool_call_id: &'a str,
    /// 工具名
    pub tool_name: &'a str,
    /// 执行结果（改写阶段为前序拦截器改写后的累积结果）
    pub result: &'a ToolResult,
    /// 当前是否被视为错误
    pub is_error: bool,
}

/// `on_tool_execution_end` 的改写；字段逐项覆盖，未设置的保持原值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolExecutionOverride {
    /// 替换结果内容
    pub content: Option<Vec<UserContent>>,
    /// 替换结构化详情
    pub details: Option<serde_json::Value>,
    /// 覆盖错误标记
    pub is_error: Option<bool>,
    /// 覆盖提前终止提示
    pub terminate: Option<bool>,
}

/// agent 事件拦截器；所有方法默认空实现。
///
/// 多个拦截器按 builder 插入序执行：门控（[`Self::on_tool_execution_start`]）
/// 首个 `Block` 短路（deny-wins）；改写（[`Self::on_tool_execution_end`]）为
/// pipeline——后一个拦截器看到前一个改写后的累积结果。
#[async_trait]
pub trait AgentInterceptor: Send + Sync {
    /// 工具参数校验通过后、执行前调用；返回 [`ToolCallDecision::Block`] 则
    /// 跳过执行，`reason` 作为错误工具结果回喂模型。
    async fn on_tool_execution_start(&self, _event: &ToolExecutionStart<'_>) -> ToolCallDecision {
        ToolCallDecision::Allow
    }

    /// 工具执行完成后调用，可改写结果（pipeline：看到的是累积结果）。
    async fn on_tool_execution_end(
        &self,
        _event: &ToolExecutionEnd<'_>,
    ) -> Option<ToolExecutionOverride> {
        None
    }
}

/// 空拦截器（默认，等价于不设任何拦截器）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopInterceptor;

impl AgentInterceptor for NoopInterceptor {}
