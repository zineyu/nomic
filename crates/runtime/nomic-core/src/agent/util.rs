//! agent 内部辅助（自 `agent.rs` 拆出的子模块，保持 `agent.rs` 在行数上限内）。

use nomic_ai::{
    ImageContent, Message, TextContent, ToolCall, UserContent, UserMessage, UserMessageContent,
    now_millis,
};

use crate::tool::{DynTool, ToolResult};

/// 一次工具调用的最终结局。
#[derive(Debug)]
pub(super) struct FinalizedToolCall {
    pub(super) tool_call: ToolCall,
    pub(super) result: ToolResult,
    pub(super) is_error: bool,
}

/// 预备完成的工具调用：门控未通过时是即时失败结果（不再执行），
/// 否则待执行。用枚举而非 `Result`：拒绝不是错误路径，是分支之一。
pub(super) enum PreparedToolCall<'a> {
    /// 待执行（工具调用 + 已解析的工具实现）
    Ready(&'a ToolCall, DynTool),
    /// 门控拒绝（工具不存在 / 拦截器拦截）的即时失败结果
    Rejected(FinalizedToolCall),
}

/// 构建 user 消息：有图片附件时为内容块列表（图片块在前、文本块在后，
/// 与 Anthropic 官方建议的排序一致）；空附件为纯文本。prompt 提交与
/// 运行中注入共用同一口径。
pub(super) fn user_message(text: &str, images: &[ImageContent]) -> Message {
    let content = if images.is_empty() {
        UserMessageContent::Text(text.to_string())
    } else {
        let mut blocks: Vec<UserContent> = images.iter().cloned().map(UserContent::Image).collect();
        blocks.push(UserContent::Text(TextContent {
            text: text.to_string(),
            text_signature: None,
        }));
        UserMessageContent::Blocks(blocks)
    };
    Message::User(UserMessage {
        content,
        timestamp: now_millis(),
    })
}
