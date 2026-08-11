//! agent 生命周期 hooks（借鉴 pi 的可选闭包，改为 trait + 默认空实现）。
//!
//! M1 只提供工具调用前后的挂点（权限门控、结果改写的位置）；
//! turn 后停止等留待后续里程碑（ADR-0001；统一消息队列见 ADR-0014）。

use async_trait::async_trait;
use nomic_ai::{AssistantMessage, ToolCall};

use crate::tool::ToolResult;

/// `before_tool_call` 的决策。
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

/// `before_tool_call` 的上下文。
#[derive(Debug)]
pub struct BeforeToolCall<'a> {
    /// 发起工具调用的 assistant 消息
    pub assistant_message: &'a AssistantMessage,
    /// 工具调用块
    pub tool_call: &'a ToolCall,
}

/// `after_tool_call` 的上下文。
#[derive(Debug)]
pub struct AfterToolCall<'a> {
    /// 发起工具调用的 assistant 消息
    pub assistant_message: &'a AssistantMessage,
    /// 工具调用块
    pub tool_call: &'a ToolCall,
    /// 执行结果（hook 可改写）
    pub result: &'a ToolResult,
    /// 当前是否被视为错误
    pub is_error: bool,
}

/// `after_tool_call` 的改写；字段逐项覆盖，未设置的保持原值。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AfterToolCallOverride {
    /// 替换结果内容
    pub content: Option<Vec<nomic_ai::UserContent>>,
    /// 替换结构化详情
    pub details: Option<serde_json::Value>,
    /// 覆盖错误标记
    pub is_error: Option<bool>,
    /// 覆盖提前终止提示
    pub terminate: Option<bool>,
}

/// agent hooks；所有方法默认空实现。
#[async_trait]
pub trait AgentHooks: Send + Sync {
    /// 工具参数校验通过后、执行前调用。
    async fn before_tool_call(&self, _ctx: &BeforeToolCall<'_>) -> ToolCallDecision {
        ToolCallDecision::Allow
    }

    /// 工具执行完成后调用，可改写结果。
    async fn after_tool_call(&self, _ctx: &AfterToolCall<'_>) -> Option<AfterToolCallOverride> {
        None
    }
}

/// 空 hooks（默认）。
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopHooks;

impl AgentHooks for NoopHooks {}
