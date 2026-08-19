//! agent 生命周期事件（自 `agent.rs` 拆出的子模块，保持 `agent.rs` 在行数上限内）。

use nomic_ai::{AssistantEvent, AssistantMessage, Message, ToolResultMessage, Usage};

use crate::tool::{ToolResult, ToolUpdate};

/// agent 生命周期事件。
///
/// 派生 serde（web 模式经 WebSocket 序列化给前端重建消息流）；
/// 字段负载（`Message` / `ToolResult` / `AssistantEvent`）均已可序列化。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum AgentEvent {
    /// 一次 prompt 运行开始
    AgentStart,
    /// 一次 prompt 运行结束（携带本次新增的所有消息）
    AgentEnd {
        /// 本次运行新增的消息
        messages: Vec<Message>,
        /// 运行结束时的权威上下文 token 估算（`estimate_context_tokens`，
        /// 与末尾 `MessageEnd` 同口径；交互端状态栏直接采用，不自行估算）
        context_tokens: u64,
    },
    /// 一个 turn 开始（一个 turn = 一次 assistant 响应 + 工具调用/结果）
    TurnStart,
    /// 一个 turn 结束
    TurnEnd {
        /// assistant 消息
        message: Box<AssistantMessage>,
        /// 本 turn 产生的工具结果
        tool_results: Vec<ToolResultMessage>,
    },
    /// 消息开始（user / assistant / toolResult）。
    ///
    /// assistant 消息的 `MessageStart` / `MessageEnd` 始终配对：provider 在
    /// 流建立前失败（重试耗尽等）不发 `Start` 事件时，由 agent 补发。
    MessageStart(Box<Message>),
    /// assistant 流式更新（携带 provider 层的增量事件）
    MessageUpdate(AssistantEvent),
    /// 消息完成（附带该消息落史后的权威上下文 token 估算：
    /// `estimate_context_tokens`，usage 锚点规则唯一定义在 core）
    MessageEnd {
        /// 完成的消息
        message: Box<Message>,
        /// 该消息落史后的上下文 token 估算
        context_tokens: u64,
    },
    /// 上下文压缩开始（自动阈值或 `/compact` 手动触发）
    CompactionStart {
        /// 压缩前的上下文 token 估算
        tokens_before: u64,
    },
    /// 上下文压缩完成（历史已替换为 摘要 + 近期保留消息）
    CompactionEnd {
        /// 结构化摘要
        summary: String,
        /// 压缩前的上下文 token 估算
        tokens_before: u64,
        /// 压缩后的权威上下文 token 估算（与 `tokens_before` 同一口径）
        context_tokens: u64,
        /// 保留的近期消息条数（session 落库 compaction entry 用）
        kept_count: usize,
        /// 摘要请求的 token 用量
        usage: Usage,
    },
    /// 工具执行开始
    ToolExecutionStart {
        /// 工具调用 id
        tool_call_id: String,
        /// 工具名
        tool_name: String,
        /// 原始参数
        args: serde_json::Value,
    },
    /// 工具执行进度
    ToolExecutionUpdate {
        /// 工具调用 id
        tool_call_id: String,
        /// 工具名
        tool_name: String,
        /// 部分结果
        partial: ToolUpdate,
    },
    /// 工具执行结束
    ToolExecutionEnd {
        /// 工具调用 id
        tool_call_id: String,
        /// 工具名
        tool_name: String,
        /// 执行结果
        result: ToolResult,
        /// 是否为错误结果
        is_error: bool,
    },
}
