//! 运行中注入（turn 边界转向）的注入点（ADR-0013/0014 的 steering 语义）。
//!
//! agent 在每个完成的 turn（当前 assistant turn 的工具调用执行完后、
//! 下一次 LLM 调用前）询问一次 [`TurnInjection::next_message`]：返回
//! `Some` 则把该消息作为 user 消息注入当前 run 并继续 loop（one-at-a-time
//! 由实现方保证），返回 `None` 且无更多工具调用时 run 按常规终止；run
//! 异常结束（取消/失败）时不询问注入点，消息由交互端保留。
//!
//! 注入队列的存储、编辑与冻结等交互语义一律由实现方负责（如 TUI 的
//! 统一消息队列），core 只提供注入点——这使 core 不依赖任何特定交互端
//! 的排队实现。

use nomic_ai::ImageContent;

/// 运行中注入的一条消息：文本 + 图片附件（图片块在前、文本块在后，
/// 与 prompt 附件同一口径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnMessage {
    /// 消息文本
    pub text: String,
    /// 图片附件
    pub images: Vec<ImageContent>,
}

/// 运行中注入源：每个完成的 turn 边界提供下一条要注入的 user 消息。
///
/// 实现方自行决定 one-at-a-time、队列编辑、冻结等语义；core 只在 turn
/// 边界调用 [`TurnInjection::next_message`] 一次。
pub trait TurnInjection: Send + Sync {
    /// 每个完成的 turn 边界调用一次；`None` 表示没有更多注入。
    fn next_message(&self) -> Option<TurnMessage>;
}
