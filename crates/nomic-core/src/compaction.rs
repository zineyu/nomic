//! 上下文压缩（compaction）：借鉴 pi-agent-core 的 `harness/compaction`，
//! 当对话上下文逼近模型窗口时，把较早的消息段压缩为结构化摘要，
//! 用一条合成 user 消息替换，保留近期消息原样。
//!
//! 忠实复刻 pi 的关键算法与模型契约：
//! - token 估算：chars/4（图片按 4800 chars 折算）；
//!   有 usage 时以最后一次 assistant 响应的实际用量为锚点累加尾部估算
//! - 触发条件：`context_tokens > context_window - reserve_tokens`
//! - 切点：从最新往前累计到 `keep_recent_tokens`，只切在 user/assistant
//!   边界（永不切在 toolResult 前，避免工具调用与结果分离）
//! - 摘要 prompt：结构化 checkpoint 格式（Goal / Progress / Key Decisions /
//!   Next Steps / Critical Context），二次压缩走 UPDATE 变体
//! - 对话序列化：`[User]:` / `[Assistant]:` / `[Assistant tool calls]:` /
//!   `[Tool result]:`（结果截断 2000 chars），防止模型把摘要请求当成对话继续
//! - 文件操作追踪：从 read/write/edit 工具调用确定性提取
//!   `<read-files>` / `<modified-files>` 附加到摘要末尾
//!
//! 与 pi 的偏离（见 docs/adr/0005）：
//! - split turn（单轮超长切在 assistant 边界）不单独生成 turn prefix 摘要，
//!   整段一次摘要
//! - 无 prompt caching，摘要请求无 cacheRetention 语义
//! - API 返回 context-overflow 错误时的压缩重试（overflow recovery）不做

use std::collections::BTreeSet;
use std::sync::Arc;

use nomic_ai::{
    AssistantContent, Context, Message, Model, Provider, StreamOptions, Usage, UserContent,
    UserMessage, UserMessageContent, extract_summary, now_millis,
};
use tokio_util::sync::CancellationToken;

// 摘要消息的构造与识别在 nomic-ai（消息模型层）定义，nomic-session 重建上下文时
// 共享同一实现；此处 re-export 保持 nomic-core 的公开 API 不变。
pub use nomic_ai::{is_summary_message, summary_message};

/// 压缩配置（默认值与 pi 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionSettings {
    /// 是否启用自动压缩（手动 `/compact` 不受此开关影响）
    pub enabled: bool,
    /// 为模型响应预留的 token 数
    pub reserve_tokens: u64,
    /// 保留不压缩的近期 token 数（估算口径）
    pub keep_recent_tokens: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
        }
    }
}

/// 压缩错误（摘要请求的失败经 provider 错误契约编码进流，这里还原为 `Err`）。
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    /// 摘要请求被取消
    #[error("summarization aborted: {0}")]
    Aborted(String),
    /// 摘要请求失败（历史保持不变，fail-safe）
    #[error("summarization failed: {0}")]
    Summarization(String),
}

/// 一次压缩的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Compaction {
    /// 结构化摘要（含 `<read-files>` / `<modified-files>` 附加段）
    pub summary: String,
    /// 压缩前的上下文 token 估算
    pub tokens_before: u64,
    /// 保留的近期消息条数（重建语义见 nomic-session 的 compaction entry）
    pub kept_count: usize,
    /// 摘要请求的 token 用量
    pub usage: Usage,
}

/// 摘要请求的系统提示词（逐字复刻 pi 的 `SUMMARIZATION_SYSTEM_PROMPT`）。
const SUMMARIZATION_SYSTEM_PROMPT: &str = "You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.
Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.";

/// 首次压缩的摘要指令（逐字复刻 pi 的 `SUMMARIZATION_PROMPT`）。
const SUMMARIZATION_PROMPT: &str = "The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.
Use this EXACT format:
## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]
## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]
## Progress
### Done
- [x] [Completed tasks/changes]
### In Progress
- [ ] [Current work]
### Blocked
- [Issues preventing progress, if any]
## Key Decisions
- **[Decision]**: [Brief rationale]
## Next Steps
1. [Ordered list of what should happen next]
## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]
Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// 二次压缩的增量更新指令（逐字复刻 pi 的 `UPDATE_SUMMARIZATION_PROMPT`）。
const UPDATE_SUMMARIZATION_PROMPT: &str = "The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.
Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it
Use this EXACT format:
## Goal
[Preserve existing goals, add new ones if the task expanded]
## Constraints & Preferences
- [Preserve existing, add new ones discovered]
## Progress
### Done
- [x] [Include previously done items AND newly completed items]
### In Progress
- [ ] [Current work - update based on progress]
### Blocked
- [Current blockers - remove if resolved]
## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)
## Next Steps
1. [Update based on current state]
## Critical Context
- [Preserve important context, add new if needed]
Keep each section concise. Preserve exact file paths, function names, and error messages.";

/// 序列化时工具结果的截断上限（与 pi 的 `TOOL_RESULT_MAX_CHARS` 一致）。
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// 图片内容的估算字符数（与 pi 的 `ESTIMATED_IMAGE_CHARS` 一致）。
const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// 取 user / toolResult 内容的文本部分（图片折算为占位长度在估算中处理）。
fn content_text(content: &[UserContent]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            UserContent::Text(text) => Some(text.text.as_str()),
            UserContent::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 估算一条消息的 token 数：chars/4（与 pi 的 `estimateTokens` 一致）。
fn estimate_tokens(message: &Message) -> u64 {
    let chars = match message {
        Message::User(user) => match &user.content {
            UserMessageContent::Text(text) => text.len(),
            UserMessageContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    UserContent::Text(text) => text.text.len(),
                    UserContent::Image(_) => ESTIMATED_IMAGE_CHARS,
                })
                .sum(),
        },
        Message::Assistant(assistant) => assistant
            .content
            .iter()
            .map(|block| match block {
                AssistantContent::Text(text) => text.text.len(),
                AssistantContent::Thinking(thinking) => thinking.thinking.len(),
                AssistantContent::ToolCall(call) => {
                    call.name.len() + call.arguments.to_string().len()
                }
            })
            .sum(),
        Message::ToolResult(result) => result
            .content
            .iter()
            .map(|block| match block {
                UserContent::Text(text) => text.text.len(),
                UserContent::Image(_) => ESTIMATED_IMAGE_CHARS,
            })
            .sum(),
    };
    (chars / 4) as u64
}

/// 一次 assistant 响应代表的总上下文 token（与 pi 的 `calculateContextTokens` 一致）。
const fn usage_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

/// 估算当前上下文的总 token 数（与 pi 的 `estimateContextTokens` 一致）：
/// 以最后一次有效 assistant 响应的实际 usage 为锚点，累加其后消息的估算；
/// 没有可用 usage 时全部按 chars/4 估算。
pub fn estimate_context_tokens(messages: &[Message]) -> u64 {
    let anchor = messages.iter().rposition(|message| {
        matches!(
            message,
            Message::Assistant(assistant)
                if !matches!(
                    assistant.stop_reason,
                    nomic_ai::StopReason::Error | nomic_ai::StopReason::Aborted
                ) && usage_tokens(&assistant.usage) > 0
        )
    });
    let Some(index) = anchor else {
        return messages.iter().map(estimate_tokens).sum();
    };
    let Message::Assistant(assistant) = &messages[index] else {
        unreachable!("anchor is an assistant message");
    };
    usage_tokens(&assistant.usage)
        + messages[index + 1..]
            .iter()
            .map(estimate_tokens)
            .sum::<u64>()
}

/// 是否应触发自动压缩（与 pi 的 `shouldCompact` 一致；
/// `context_window == 0` 表示规格未知，不触发）。
pub const fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionSettings,
) -> bool {
    settings.enabled
        && context_window > 0
        && context_tokens > context_window.saturating_sub(settings.reserve_tokens)
}

/// 寻找切点：返回首个被保留消息的索引，`messages[start..cut]` 进入摘要。
///
/// 从最新往前累计估算 token，达到 `keep_recent_tokens` 后切在最近的
/// user/assistant 边界（toolResult 永不做切点，保证工具调用与结果同侧）。
/// 无可切内容（`cut <= start`）时返回 `None`。
pub fn find_cut_point(
    messages: &[Message],
    start: usize,
    keep_recent_tokens: u64,
) -> Option<usize> {
    let candidates: Vec<usize> = (start..messages.len())
        .filter(|&index| matches!(messages[index], Message::User(_) | Message::Assistant(_)))
        .collect();
    let mut cut = *candidates.first()?;
    let mut accumulated = 0_u64;
    for index in (start..messages.len()).rev() {
        accumulated += estimate_tokens(&messages[index]);
        if accumulated >= keep_recent_tokens {
            if let Some(&point) = candidates.iter().find(|&&point| point >= index) {
                cut = point;
            }
            break;
        }
    }
    (cut > start).then_some(cut)
}

/// 把消息序列化为纯文本对话（与 pi 的 `serializeConversation` 一致），
/// 防止模型把摘要请求当成对话继续。工具结果截断到 2000 chars。
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                let text = match &user.content {
                    UserMessageContent::Text(text) => text.clone(),
                    UserMessageContent::Blocks(blocks) => content_text(blocks),
                };
                if !text.is_empty() {
                    parts.push(format!("[User]: {text}"));
                }
            }
            Message::Assistant(assistant) => {
                let thinking: Vec<&str> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::Thinking(thinking) => Some(thinking.thinking.as_str()),
                        _ => None,
                    })
                    .collect();
                if !thinking.is_empty() {
                    parts.push(format!("[Assistant thinking]: {}", thinking.join("\n")));
                }
                let text: Vec<&str> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect();
                if !text.is_empty() {
                    parts.push(format!("[Assistant]: {}", text.join("\n")));
                }
                let calls: Vec<String> = assistant
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        AssistantContent::ToolCall(call) => {
                            let args = call.arguments.as_object().map_or_else(
                                || call.arguments.to_string(),
                                |object| {
                                    object
                                        .iter()
                                        .map(|(key, value)| format!("{key}={value}"))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                },
                            );
                            Some(format!("{}({args})", call.name))
                        }
                        _ => None,
                    })
                    .collect();
                if !calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let text = content_text(&result.content);
                if !text.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&text, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
        }
    }
    parts.join("\n")
}

/// 截断并标注被截掉的字符数（与 pi 的 `truncateForSummary` 一致）。
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let mut index = max_chars;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    format!(
        "{}\n[... {} more characters truncated]",
        &text[..index],
        text.len() - index
    )
}

/// 从工具调用与既有摘要中确定性提取的文件操作（与 pi 的
/// `extractFileOperations` 一致；修改过的文件从只读列表剔除）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileOps {
    /// 读过但未修改的文件（排序去重）
    pub read_files: BTreeSet<String>,
    /// 写过或编辑过的文件（排序去重）
    pub modified_files: BTreeSet<String>,
}

/// 累积待摘要消息段中的 read/write/edit 路径，并合并前次摘要里记录的
/// 文件清单（跨多次压缩累计，与 pi 经 `details` 累计等效）。
pub fn extract_file_ops(messages: &[Message], previous_summary: Option<&str>) -> FileOps {
    let mut read = BTreeSet::new();
    let mut modified = BTreeSet::new();
    if let Some(summary) = previous_summary {
        parse_file_list(summary, "read-files", &mut read);
        parse_file_list(summary, "modified-files", &mut modified);
    }
    for message in messages {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        for block in &assistant.content {
            let AssistantContent::ToolCall(call) = block else {
                continue;
            };
            let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            match call.name.as_str() {
                "read" => {
                    read.insert(path.to_string());
                }
                "write" | "edit" => {
                    modified.insert(path.to_string());
                }
                _ => {}
            }
        }
    }
    let read_only: BTreeSet<_> = read.difference(&modified).cloned().collect();
    FileOps {
        read_files: read_only,
        modified_files: modified,
    }
}

/// 解析前次摘要中 `<tag>\n路径…\n</tag>` 形式的文件清单。
fn parse_file_list(summary: &str, tag: &str, out: &mut BTreeSet<String>) {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = summary.find(&open).map(|index| index + open.len()) else {
        return;
    };
    let Some(end) = summary[start..].find(&close).map(|index| index + start) else {
        return;
    };
    for line in summary[start..end].lines() {
        let line = line.trim();
        if !line.is_empty() {
            out.insert(line.to_string());
        }
    }
}

/// 把文件清单格式化为附加到摘要末尾的段落（与 pi 的 `formatFileOperations` 一致）。
fn format_file_ops(file_ops: &FileOps) -> String {
    let mut sections = Vec::new();
    if !file_ops.read_files.is_empty() {
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            file_ops
                .read_files
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !file_ops.modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            file_ops
                .modified_files
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

/// 调用 LLM 生成结构化摘要，返回 `(摘要文本, 用量)`。
///
/// 请求与 agent 上下文完全隔离：专用系统提示词 + 单条 user 消息
/// （序列化对话 + 指令），不携带工具；其流事件**不进入** agent 事件流。
/// 输出上限取 `min(0.8 * reserve_tokens, model.max_tokens)`（与 pi 一致）。
/// 摘要请求的目标：provider / 模型 / 基础选项 / 压缩配置。
struct SummarizeTarget<'a> {
    provider: &'a Arc<dyn Provider>,
    model: &'a Model,
    base_options: &'a StreamOptions,
    settings: &'a CompactionSettings,
}

async fn generate_summary(
    target: &SummarizeTarget<'_>,
    messages: &[Message],
    previous_summary: Option<&str>,
    custom_instructions: Option<&str>,
    cancel: CancellationToken,
) -> Result<(String, Usage), CompactionError> {
    let mut prompt = format!(
        "<conversation>\n{}\n</conversation>\n\n",
        serialize_conversation(messages)
    );
    let mut instruction = if previous_summary.is_some() {
        UPDATE_SUMMARIZATION_PROMPT
    } else {
        SUMMARIZATION_PROMPT
    }
    .to_string();
    if let Some(previous) = previous_summary {
        prompt = format!("{prompt}<previous-summary>\n{previous}\n</previous-summary>\n\n");
    }
    if let Some(instructions) = custom_instructions {
        instruction = format!("{instruction}\n\nAdditional focus: {instructions}");
    }
    prompt.push_str(&instruction);

    let max_tokens = (target.settings.reserve_tokens * 4 / 5).min(if target.model.max_tokens > 0 {
        target.model.max_tokens
    } else {
        u64::MAX
    });
    let options = StreamOptions {
        // 摘要请求不带采样温度与推理配置之外的用户选项，max_tokens 按需收窄
        temperature: None,
        max_tokens: Some(max_tokens),
        reasoning: target.base_options.reasoning,
        api_key: target.base_options.api_key.clone(),
        headers: target.base_options.headers.clone(),
        timeout_ms: target.base_options.timeout_ms,
    };
    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text(prompt),
            timestamp: now_millis(),
        })],
        tools: Vec::new(),
    };

    let mut stream = target
        .provider
        .stream(target.model, &context, &options, cancel);
    let mut final_message = None;
    while let Some(event) = stream.next().await {
        match event {
            nomic_ai::AssistantEvent::Done { message }
            | nomic_ai::AssistantEvent::Error { message } => final_message = Some(*message),
            _ => {}
        }
    }
    let message = final_message.ok_or_else(|| {
        CompactionError::Summarization("stream closed without Done/Error".to_string())
    })?;
    match message.stop_reason {
        nomic_ai::StopReason::Aborted => Err(CompactionError::Aborted(
            message.error_message.unwrap_or_default(),
        )),
        nomic_ai::StopReason::Error => Err(CompactionError::Summarization(
            message.error_message.unwrap_or_default(),
        )),
        _ => {
            let text = message
                .content
                .iter()
                .filter_map(|block| match block {
                    AssistantContent::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok((text, message.usage))
        }
    }
}

/// 压缩计划的输入快照：消息历史 + 可选的用户聚焦指令。
#[derive(Debug)]
pub struct CompactRequest<'a> {
    /// 当前完整消息历史（首条可能是前次摘要）
    pub messages: &'a [Message],
    /// `/compact <instructions>` 的用户聚焦指令
    pub custom_instructions: Option<&'a str>,
}

/// 执行一次压缩：找切点 → 生成摘要 → 组装新历史。
///
/// 返回 `Ok(None)` 表示无可压缩内容（历史太短或切点退化），历史保持不变。
/// 失败返回 `Err`，历史同样保持不变（fail-safe）。
/// `on_start` 在切点确定、摘要请求发起前回调（携带压缩前的 token 估算），
/// 供调用方发出"压缩进行中"事件。
pub async fn compact_messages(
    provider: &Arc<dyn Provider>,
    model: &Model,
    base_options: &StreamOptions,
    settings: &CompactionSettings,
    request: &CompactRequest<'_>,
    cancel: CancellationToken,
    on_start: impl FnOnce(u64),
) -> Result<Option<(Compaction, Vec<Message>)>, CompactionError> {
    let messages = request.messages;
    // 前次摘要固定在 messages[0]（in-memory 与 resume 重建的一致约定）：
    // 不参与序列化，经 <previous-summary> 走 UPDATE prompt（与 pi 一致）
    let (offset, previous_summary) = match messages.first().and_then(extract_summary) {
        Some(summary) => (1, Some(summary.to_string())),
        None => (0, None),
    };
    let Some(cut) = find_cut_point(messages, offset, settings.keep_recent_tokens) else {
        return Ok(None);
    };
    let to_summarize = &messages[offset..cut];
    if to_summarize.is_empty() {
        return Ok(None);
    }
    let tokens_before = estimate_context_tokens(messages);
    on_start(tokens_before);
    let target = SummarizeTarget {
        provider,
        model,
        base_options,
        settings,
    };
    let (text, usage) = generate_summary(
        &target,
        to_summarize,
        previous_summary.as_deref(),
        request.custom_instructions,
        cancel,
    )
    .await?;
    let file_ops = extract_file_ops(to_summarize, previous_summary.as_deref());
    let summary = format!("{text}{}", format_file_ops(&file_ops));

    let kept_count = messages.len() - cut;
    let mut new_history = Vec::with_capacity(kept_count + 1);
    new_history.push(summary_message(&summary, now_millis()));
    new_history.extend_from_slice(&messages[cut..]);

    let compaction = Compaction {
        summary,
        tokens_before,
        kept_count,
        usage,
    };
    Ok(Some((compaction, new_history)))
}

#[cfg(test)]
mod tests {
    use nomic_ai::{AssistantMessage, StopReason, TextContent, ToolCall, ToolResultMessage};

    use super::*;

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_string()),
            timestamp: 0,
        })
    }

    fn assistant(blocks: Vec<AssistantContent>) -> Message {
        Message::Assistant(AssistantMessage {
            content: blocks,
            api: nomic_ai::ApiKind::OpenAiCompletions,
            provider: "mock".to_string(),
            model: "mock".to_string(),
            response_model: None,
            response_id: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        })
    }

    fn text_block(text: &str) -> AssistantContent {
        AssistantContent::Text(TextContent {
            text: text.to_string(),
            text_signature: None,
        })
    }

    fn tool_call_block(name: &str, args: serde_json::Value) -> AssistantContent {
        AssistantContent::ToolCall(ToolCall {
            id: "c1".to_string(),
            name: name.to_string(),
            arguments: args,
            thought_signature: None,
        })
    }

    fn tool_result(text: &str) -> Message {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "c1".to_string(),
            tool_name: "read".to_string(),
            content: vec![UserContent::Text(TextContent {
                text: text.to_string(),
                text_signature: None,
            })],
            details: None,
            is_error: false,
            timestamp: 0,
        })
    }

    fn with_usage(mut message: Message, total_tokens: u64) -> Message {
        if let Message::Assistant(assistant) = &mut message {
            assistant.usage.total_tokens = total_tokens;
        }
        message
    }

    // ── token 估算 ─────────────────────────────────────────────────────────

    #[test]
    fn estimate_context_tokens_falls_back_to_char_estimate_without_usage() {
        let messages = vec![user("12345678"), assistant(vec![text_block("1234")])];
        // 8/4 + 4/4 = 3
        assert_eq!(estimate_context_tokens(&messages), 3);
    }

    #[test]
    fn estimate_context_tokens_anchors_at_last_valid_usage_plus_trailing() {
        let messages = vec![
            user("ignored"),
            with_usage(assistant(vec![text_block("x")]), 1000),
            user("12345678"),    // 2 tokens trailing
            tool_result("1234"), // 1 token trailing
        ];
        assert_eq!(estimate_context_tokens(&messages), 1003);
    }

    #[test]
    fn estimate_context_tokens_skips_error_and_zero_usage_assistants() {
        let mut error_assistant = assistant(vec![text_block("x")]);
        if let Message::Assistant(a) = &mut error_assistant {
            a.stop_reason = StopReason::Error;
            a.usage.total_tokens = 5000;
        }
        let messages = vec![
            with_usage(assistant(vec![text_block("x")]), 100),
            error_assistant,
            user("12345678"), // 2 tokens trailing after the *valid* anchor
        ];
        // 锚点是第一条 assistant（error 的不算）；error assistant 按 chars 估算进 trailing
        // trailing = error assistant (1/4=0) + user (2) = 2
        assert_eq!(estimate_context_tokens(&messages), 102);
    }

    #[test]
    fn should_compact_respects_threshold_window_and_enabled() {
        let settings = CompactionSettings {
            enabled: true,
            reserve_tokens: 100,
            keep_recent_tokens: 10,
        };
        assert!(should_compact(901, 1000, &settings));
        assert!(!should_compact(900, 1000, &settings));
        assert!(!should_compact(5000, 0, &settings), "窗口未知不触发");
        // 窗口小于预留时阈值饱和为 0：有内容即触发（不能下溢）
        assert!(should_compact(
            5000,
            8192,
            &CompactionSettings {
                reserve_tokens: 16_384,
                ..settings
            }
        ));
        let disabled = CompactionSettings {
            enabled: false,
            ..settings
        };
        assert!(!should_compact(5000, 1000, &disabled));
    }

    // ── 切点 ───────────────────────────────────────────────────────────────

    #[test]
    fn cut_point_never_lands_on_tool_result() {
        // 每条消息 40 chars = 10 tokens；keep_recent=25 需要保留 3 条
        let messages = vec![
            user(&"u".repeat(40)),
            assistant(vec![text_block(&"a".repeat(40))]),
            tool_result(&"r".repeat(40)),
            assistant(vec![text_block(&"a".repeat(40))]),
            tool_result(&"r".repeat(40)),
        ];
        let cut = find_cut_point(&messages, 0, 25).expect("cut");
        assert!(
            matches!(messages[cut], Message::User(_) | Message::Assistant(_)),
            "切点必须在 user/assistant 边界"
        );
        // 累计到最后一条 assistant（含）时 10+10+10=30 >= 25，切在它前面
        assert_eq!(cut, 3);
    }

    #[test]
    fn cut_point_returns_none_when_nothing_to_summarize() {
        let messages = vec![user("short"), assistant(vec![text_block("reply")])];
        // 总估算远低于预算时切点退化为 start → None
        assert_eq!(find_cut_point(&messages, 0, 10_000), None);
    }

    #[test]
    fn cut_point_skips_summary_prefix_offset() {
        let summary = summary_message("previous work", 0);
        let messages = vec![
            summary,
            user(&"u".repeat(400)),                        // 100 tokens
            assistant(vec![text_block(&"a".repeat(400))]), // 100 tokens
            user(&"u".repeat(400)),                        // 100 tokens
        ];
        // start=1；keep 150 → 从尾部累计第二条 user 处达 200，切在 index 2
        let cut = find_cut_point(&messages, 1, 150).expect("cut");
        assert_eq!(cut, 2);
        // start 之后无内容可摘要时 None
        assert_eq!(find_cut_point(&messages[..2], 1, 150), None);
    }

    // ── 序列化 ─────────────────────────────────────────────────────────────

    #[test]
    fn serialize_conversation_formats_each_role() {
        let messages = vec![
            user("hello"),
            assistant(vec![
                AssistantContent::Thinking(nomic_ai::ThinkingContent {
                    thinking: "hmm".to_string(),
                    thinking_signature: None,
                    redacted: false,
                }),
                text_block("let me read"),
                tool_call_block("read", serde_json::json!({"path": "a.rs", "offset": 1})),
                tool_call_block("bash", serde_json::json!({"command": "ls"})),
            ]),
            tool_result("file contents"),
        ];
        let text = serialize_conversation(&messages);
        assert!(text.contains("[User]: hello"), "{text}");
        assert!(text.contains("[Assistant thinking]: hmm"), "{text}");
        assert!(text.contains("[Assistant]: let me read"), "{text}");
        assert!(
            text.contains(
                "[Assistant tool calls]: read(offset=1, path=\"a.rs\"); bash(command=\"ls\")"
            ),
            "{text}"
        );
        assert!(text.contains("[Tool result]: file contents"), "{text}");
    }

    #[test]
    fn serialize_truncates_tool_results_with_marker() {
        let long = "x".repeat(3000);
        let text = serialize_conversation(&[tool_result(&long)]);
        assert!(
            text.contains("[... 1000 more characters truncated]"),
            "{text}"
        );
        assert!(!text.contains(&"x".repeat(2001)));
    }

    // ── 文件操作提取 ─────────────────────────────────────────────────────────

    #[test]
    fn file_ops_from_tool_calls_and_previous_summary() {
        let messages = vec![assistant(vec![
            tool_call_block("read", serde_json::json!({"path": "src/a.rs"})),
            tool_call_block("read", serde_json::json!({"path": "src/b.rs"})),
            tool_call_block("edit", serde_json::json!({"path": "src/b.rs"})),
            tool_call_block("write", serde_json::json!({"path": "src/c.rs"})),
            tool_call_block("bash", serde_json::json!({"command": "touch x"})),
        ])];
        let previous = "text\n<read-files>\nsrc/old.rs\nsrc/b.rs\n</read-files>\n<modified-files>\nsrc/d.rs\n</modified-files>";
        let ops = extract_file_ops(&messages, Some(previous));
        // b.rs 被修改 → 从只读剔除；bash 无 path 语义；前次清单合并
        let read: Vec<_> = ops.read_files.iter().cloned().collect();
        let modified: Vec<_> = ops.modified_files.iter().cloned().collect();
        assert_eq!(read, vec!["src/a.rs", "src/old.rs"]);
        assert_eq!(modified, vec!["src/b.rs", "src/c.rs", "src/d.rs"]);

        let formatted = format_file_ops(&ops);
        assert!(formatted.contains("<read-files>\nsrc/a.rs\nsrc/old.rs\n</read-files>"));
        assert!(formatted.contains("<modified-files>"));
        assert!(format_file_ops(&FileOps::default()).is_empty());
    }

    // ── 摘要消息 ────────────────────────────────────────────────────────────

    #[test]
    fn summary_message_roundtrips_through_marker() {
        let message = summary_message("## Goal\ndo stuff", 0);
        assert!(is_summary_message(&message));
        assert_eq!(extract_summary(&message), Some("## Goal\ndo stuff"));
        assert!(!is_summary_message(&user(
            "The conversation history before this point"
        )));
    }
}
