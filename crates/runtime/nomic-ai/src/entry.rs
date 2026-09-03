//! 统一历史条目模型：会话历史在**存储层**的唯一 payload 格式
//! （见 `docs/adr/0045`）。
//!
//! [`Entry`] = 角色 + 有序内容块（[`Part`]）+ 角色元数据 + 时间戳，
//! 四种角色（user / assistant / tool_result / compaction）共用一种
//! 序列化形状；compaction 由此从旁路 payload（`entries.kind =
//! 'compaction'` 时代的独立 JSON）升级为一等角色。
//!
//! 本模型只负责**持久化与重放**：agent 持有的内存历史仍是
//! [`Message`]，provider wire 转换也不经手 [`Entry`]。两侧的
//! 结构同构由 [`Entry::from`]（Message → Entry，存储方向）与
//! [`Entry::to_message`]（Entry → Message，重放方向）维系，转换规则
//! 集中在本模块，是全库唯一的格式定义点。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::{
    ApiKind, AssistantContent, AssistantMessage, ImageContent, Message, StopReason, TextContent,
    ThinkingContent, ToolCall, ToolResultMessage, Usage, UserContent, UserMessage,
    UserMessageContent,
};

/// 历史条目角色（serde 值与 `entries.role` 列的既有词表一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryRole {
    /// 用户消息
    User,
    /// assistant 响应
    Assistant,
    /// 工具执行结果
    ToolResult,
    /// 上下文压缩记录
    Compaction,
}

impl EntryRole {
    /// 角色词（与 serde 序列化同一份 snake_case 词表）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::ToolResult => "tool_result",
            Self::Compaction => "compaction",
        }
    }
}

impl fmt::Display for EntryRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 工具执行结果内容块（tool_result 条目携带的结果本体）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultPart {
    /// 对应的工具调用 id
    pub tool_call_id: String,
    /// 工具名
    pub tool_name: String,
    /// 结果内容（按约定仅 [`Part::Text`] / [`Part::Image`]）
    pub content: Vec<Part>,
    /// 结构化详情（日志与 UI 渲染用，不进 LLM 上下文）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// 是否为错误结果（同样回喂模型，由模型自我修正）
    pub is_error: bool,
}

/// 上下文压缩记录（compaction 条目的内容块）。
///
/// 记录一次上下文压缩的结果：摘要正文、保留的近期消息条数与压缩前的
/// token 估算。重建语义（`kept_count` 相对计数代替 pi 的
/// `first_kept_entry_id` 绝对指针、重复压缩的递归成立性、分支路径重放的
/// 精确性）唯一定义于 [`crate::compaction`] module（见 `docs/adr/0005`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionRecord {
    /// 结构化摘要（含 `<read-files>` / `<modified-files>` 附加段）
    pub summary: String,
    /// 压缩时保留的近期消息条数（相对压缩前的有效上下文计数）
    pub kept_count: u64,
    /// 压缩前的上下文 token 估算
    pub tokens_before: u64,
}

/// entry 的内容块：统一 part 词汇表，替代 `UserContent` /
/// `AssistantContent` 双 enum（二者是同一词汇表按角色的切片）。
///
/// 各角色允许的 part 种类由 [`Entry::from`] / [`Entry::to_message`]
/// 的转换规则约定（构造侧保证，反序列化保持宽容以便前向兼容）：
/// user → Text / Image；assistant → Text / Thinking / ToolCall；
/// tool_result → 恰好一个 ToolResult；compaction → 恰好一个 Compaction。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    /// 文本
    Text(TextContent),
    /// 推理/思考
    Thinking(ThinkingContent),
    /// 图片（base64 内联）
    Image(ImageContent),
    /// 工具调用
    ToolCall(ToolCall),
    /// 工具执行结果
    ToolResult(ToolResultPart),
    /// 上下文压缩记录
    Compaction(CompactionRecord),
}

impl Part {
    /// part 种类词（与 serde 序列化同一份 snake_case 词表，错误消息用）。
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Thinking(_) => "thinking",
            Self::Image(_) => "image",
            Self::ToolCall(_) => "tool_call",
            Self::ToolResult(_) => "tool_result",
            Self::Compaction(_) => "compaction",
        }
    }
}

/// assistant 条目的响应元数据（一次响应一份，挂在条目上而非 part 上）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseMeta {
    /// 发起请求的 API 种类
    pub api: ApiKind,
    /// provider 标识
    pub provider: String,
    /// 请求时使用的模型 id
    pub model: String,
    /// 响应中实际返回的模型 id（如 OpenRouter auto 路由），与请求不同时记录
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// provider 侧的响应 id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// token 用量
    pub usage: Usage,
    /// 终止原因
    pub stop_reason: StopReason,
    /// 错误信息（`stop_reason` 为 `Error` / `Aborted` 时存在）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// 统一历史条目：所有持久化 payload 的唯一格式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// 条目角色
    pub role: EntryRole,
    /// 有序内容块
    pub parts: Vec<Part>,
    /// assistant 条目的响应元数据（其余角色为 `None`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponseMeta>,
    /// Unix 毫秒时间戳
    pub timestamp: u64,
}

/// Entry → Message 转换失败（role/parts 组合非法或元数据缺失）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryError {
    /// compaction 条目不是对话消息（用 [`Entry::compaction_record`] 提取）
    CompactionNotMessage,
    /// assistant 条目缺少响应元数据
    MissingResponseMeta,
    /// tool_result / compaction 条目缺少其必需的唯一内容块
    MissingPart(EntryRole),
    /// 角色不允许的内容块
    UnexpectedPart {
        /// 条目角色
        role: EntryRole,
        /// 实际出现的 part 种类
        part: &'static str,
    },
}

impl fmt::Display for EntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CompactionNotMessage => f.write_str("compaction 条目不是对话消息"),
            Self::MissingResponseMeta => f.write_str("assistant 条目缺少响应元数据"),
            Self::MissingPart(role) => {
                write!(f, "{role} 条目缺少必需的内容块")
            }
            Self::UnexpectedPart { role, part } => {
                write!(f, "{role} 条目不允许 {part} 内容块")
            }
        }
    }
}

impl std::error::Error for EntryError {}

impl Entry {
    /// 构造 compaction 条目（内容块为压缩记录本体）。
    #[must_use]
    pub fn compaction(record: CompactionRecord, timestamp: u64) -> Self {
        Self {
            role: EntryRole::Compaction,
            parts: vec![Part::Compaction(record)],
            response: None,
            timestamp,
        }
    }

    /// 提取 compaction 条目的压缩记录；非 compaction 条目或内容块缺失
    /// 时为 `None`。
    #[must_use]
    pub fn compaction_record(&self) -> Option<&CompactionRecord> {
        if self.role != EntryRole::Compaction {
            return None;
        }
        self.parts.iter().find_map(|part| match part {
            Part::Compaction(record) => Some(record),
            _ => None,
        })
    }

    /// 条目是否含工具调用块（assistant 条目；分支起点选择用）。
    #[must_use]
    pub fn has_tool_calls(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, Part::ToolCall(_)))
    }

    /// 重放方向转换：Entry → Message。
    ///
    /// 校验 role/parts 组合：user 仅 Text/Image；assistant 仅
    /// Text/Thinking/ToolCall 且响应元数据必须存在；tool_result 恰好一个
    /// ToolResult 块。compaction 条目报 [`EntryError::CompactionNotMessage`]
    /// （重放路径经 [`Entry::compaction_record`] 单独处理）。
    pub fn to_message(&self) -> Result<Message, EntryError> {
        match self.role {
            EntryRole::User => Ok(Message::User(UserMessage {
                content: self.user_content()?,
                timestamp: self.timestamp,
            })),
            EntryRole::Assistant => Ok(Message::Assistant(self.to_assistant()?)),
            EntryRole::ToolResult => Ok(Message::ToolResult(self.to_tool_result()?)),
            EntryRole::Compaction => Err(EntryError::CompactionNotMessage),
        }
    }

    /// user 条目内容：单个无签名 Text 块还原为 `Text` 快捷形式，否则
    /// 还原为 `Blocks`（仅 Text/Image 块合法）。
    fn user_content(&self) -> Result<UserMessageContent, EntryError> {
        if let [Part::Text(text)] = self.parts.as_slice()
            && text.text_signature.is_none()
        {
            return Ok(UserMessageContent::Text(text.text.clone()));
        }
        let blocks = self
            .parts
            .iter()
            .map(user_block)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(UserMessageContent::Blocks(blocks))
    }

    fn to_assistant(&self) -> Result<AssistantMessage, EntryError> {
        let meta = self
            .response
            .clone()
            .ok_or(EntryError::MissingResponseMeta)?;
        let content = self
            .parts
            .iter()
            .map(|part| match part {
                Part::Text(text) => Ok(AssistantContent::Text(text.clone())),
                Part::Thinking(thinking) => Ok(AssistantContent::Thinking(thinking.clone())),
                Part::ToolCall(call) => Ok(AssistantContent::ToolCall(call.clone())),
                other => Err(EntryError::UnexpectedPart {
                    role: EntryRole::Assistant,
                    part: other.kind(),
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AssistantMessage {
            content,
            api: meta.api,
            provider: meta.provider,
            model: meta.model,
            response_model: meta.response_model,
            response_id: meta.response_id,
            usage: meta.usage,
            stop_reason: meta.stop_reason,
            error_message: meta.error_message,
            timestamp: self.timestamp,
        })
    }

    fn to_tool_result(&self) -> Result<ToolResultMessage, EntryError> {
        let [Part::ToolResult(result)] = self.parts.as_slice() else {
            return if self.parts.is_empty() {
                Err(EntryError::MissingPart(EntryRole::ToolResult))
            } else {
                Err(EntryError::UnexpectedPart {
                    role: EntryRole::ToolResult,
                    part: self.parts[0].kind(),
                })
            };
        };
        let content = result
            .content
            .iter()
            .map(user_block)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ToolResultMessage {
            tool_call_id: result.tool_call_id.clone(),
            tool_name: result.tool_name.clone(),
            content,
            details: result.details.clone(),
            is_error: result.is_error,
            timestamp: self.timestamp,
        })
    }
}

/// user 侧内容块（Text/Image）提取；其余 part 种类对 user 角色非法。
fn user_block(part: &Part) -> Result<UserContent, EntryError> {
    match part {
        Part::Text(text) => Ok(UserContent::Text(text.clone())),
        Part::Image(image) => Ok(UserContent::Image(image.clone())),
        other => Err(EntryError::UnexpectedPart {
            role: EntryRole::User,
            part: other.kind(),
        }),
    }
}

/// 存储方向转换：Message → Entry（结构同构，恒成功）。
impl From<&Message> for Entry {
    fn from(message: &Message) -> Self {
        match message {
            Message::User(user) => Self {
                role: EntryRole::User,
                parts: match &user.content {
                    UserMessageContent::Text(text) => vec![Part::Text(TextContent {
                        text: text.clone(),
                        text_signature: None,
                    })],
                    UserMessageContent::Blocks(blocks) => blocks.iter().map(Into::into).collect(),
                },
                response: None,
                timestamp: user.timestamp,
            },
            Message::Assistant(assistant) => Self {
                role: EntryRole::Assistant,
                parts: assistant.content.iter().map(Into::into).collect(),
                response: Some(ResponseMeta {
                    api: assistant.api,
                    provider: assistant.provider.clone(),
                    model: assistant.model.clone(),
                    response_model: assistant.response_model.clone(),
                    response_id: assistant.response_id.clone(),
                    usage: assistant.usage,
                    stop_reason: assistant.stop_reason,
                    error_message: assistant.error_message.clone(),
                }),
                timestamp: assistant.timestamp,
            },
            Message::ToolResult(result) => Self {
                role: EntryRole::ToolResult,
                parts: vec![Part::ToolResult(ToolResultPart {
                    tool_call_id: result.tool_call_id.clone(),
                    tool_name: result.tool_name.clone(),
                    content: result.content.iter().map(Into::into).collect(),
                    details: result.details.clone(),
                    is_error: result.is_error,
                })],
                response: None,
                timestamp: result.timestamp,
            },
        }
    }
}

impl From<&UserContent> for Part {
    fn from(content: &UserContent) -> Self {
        match content {
            UserContent::Text(text) => Self::Text(text.clone()),
            UserContent::Image(image) => Self::Image(image.clone()),
        }
    }
}

impl From<&AssistantContent> for Part {
    fn from(content: &AssistantContent) -> Self {
        match content {
            AssistantContent::Text(text) => Self::Text(text.clone()),
            AssistantContent::Thinking(thinking) => Self::Thinking(thinking.clone()),
            AssistantContent::ToolCall(call) => Self::ToolCall(call.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApiKind, Cost};

    fn user_message(text: &str) -> Message {
        Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_string()),
            timestamp: 1_000,
        })
    }

    fn assistant_message() -> Message {
        Message::Assistant(AssistantMessage {
            content: vec![
                AssistantContent::Thinking(ThinkingContent {
                    thinking: "想一下".to_string(),
                    thinking_signature: Some("sig".to_string()),
                    redacted: false,
                }),
                AssistantContent::Text(TextContent {
                    text: "回答".to_string(),
                    text_signature: None,
                }),
                AssistantContent::ToolCall(ToolCall {
                    id: "call-1".to_string(),
                    name: "read".to_string(),
                    arguments: serde_json::json!({"path": "a.rs"}),
                    thought_signature: None,
                }),
            ],
            api: ApiKind::AnthropicMessages,
            provider: "anthropic".to_string(),
            model: "claude".to_string(),
            response_model: None,
            response_id: Some("resp-1".to_string()),
            usage: Usage {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: None,
                total_tokens: 15,
                cost: Cost::default(),
            },
            stop_reason: StopReason::ToolUse,
            error_message: None,
            timestamp: 2_000,
        })
    }

    fn tool_result_message() -> Message {
        Message::ToolResult(ToolResultMessage {
            tool_call_id: "call-1".to_string(),
            tool_name: "read".to_string(),
            content: vec![
                UserContent::Text(TextContent {
                    text: "文件内容".to_string(),
                    text_signature: None,
                }),
                UserContent::Image(ImageContent {
                    data: "aW1n".to_string(),
                    mime_type: "image/png".to_string(),
                }),
            ],
            details: Some(serde_json::json!({"lines": 3})),
            is_error: true,
            timestamp: 3_000,
        })
    }

    /// 三角色 Message → Entry → Message 往返恒等（含 thinking 签名、
    /// 工具调用、图片、details 等全字段）。
    #[test]
    fn message_entry_roundtrip_is_identity() {
        for message in [
            user_message("hi"),
            assistant_message(),
            tool_result_message(),
        ] {
            let entry = Entry::from(&message);
            // 存储格式必经 serde，连 serde 一起往返
            let json = serde_json::to_string(&entry).expect("serialize entry");
            let restored: Entry = serde_json::from_str(&json).expect("deserialize entry");
            assert_eq!(restored.to_message(), Ok(message));
        }
    }

    /// user 多文本块（Blocks 形式）经 Entry 往返保持 Blocks 形态，
    /// 单文本块归一为 Text 快捷形式。
    #[test]
    fn user_blocks_roundtrip_preserves_block_form() {
        let message = Message::User(UserMessage {
            content: UserMessageContent::Blocks(vec![
                UserContent::Text(TextContent {
                    text: "第一段".to_string(),
                    text_signature: None,
                }),
                UserContent::Image(ImageContent {
                    data: "aW1n".to_string(),
                    mime_type: "image/png".to_string(),
                }),
            ]),
            timestamp: 1_000,
        });
        let entry = Entry::from(&message);
        assert_eq!(entry.role, EntryRole::User);
        assert_eq!(entry.parts.len(), 2);
        assert_eq!(entry.to_message(), Ok(message));
    }

    /// compaction 条目：构造、记录提取与序列化形状；to_message 拒绝。
    #[test]
    fn compaction_entry_carries_record_as_part() {
        let record = CompactionRecord {
            summary: "摘要".to_string(),
            kept_count: 2,
            tokens_before: 12_345,
        };
        let entry = Entry::compaction(record.clone(), 9_999);
        assert_eq!(entry.role, EntryRole::Compaction);
        assert_eq!(entry.compaction_record(), Some(&record));
        assert_eq!(entry.to_message(), Err(EntryError::CompactionNotMessage));
        let json = serde_json::to_value(&entry).expect("serialize");
        assert_eq!(json["role"], "compaction");
        assert_eq!(json["parts"][0]["type"], "compaction");
        assert_eq!(json["parts"][0]["kept_count"], 2);
        assert_eq!(json["timestamp"], 9_999);
    }

    /// 非法 role/parts 组合与元数据缺失的报错路径。
    #[test]
    fn to_message_rejects_invalid_combinations() {
        // assistant 缺响应元数据
        let entry = Entry {
            role: EntryRole::Assistant,
            parts: vec![],
            response: None,
            timestamp: 0,
        };
        assert_eq!(entry.to_message(), Err(EntryError::MissingResponseMeta));

        // user 条目混入 tool_call 块
        let entry = Entry {
            role: EntryRole::User,
            parts: vec![Part::ToolCall(ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: serde_json::Value::Null,
                thought_signature: None,
            })],
            response: None,
            timestamp: 0,
        };
        assert_eq!(
            entry.to_message(),
            Err(EntryError::UnexpectedPart {
                role: EntryRole::User,
                part: "tool_call",
            })
        );

        // tool_result 条目为空
        let entry = Entry {
            role: EntryRole::ToolResult,
            parts: vec![],
            response: None,
            timestamp: 0,
        };
        assert_eq!(
            entry.to_message(),
            Err(EntryError::MissingPart(EntryRole::ToolResult))
        );
    }

    /// 角色词与 serde 序列化同源，且与 entries.role 列既有词表一致。
    #[test]
    fn entry_role_words_match_serde_and_column() {
        for (role, word) in [
            (EntryRole::User, "user"),
            (EntryRole::Assistant, "assistant"),
            (EntryRole::ToolResult, "tool_result"),
            (EntryRole::Compaction, "compaction"),
        ] {
            assert_eq!(role.as_str(), word);
            assert_eq!(role.to_string(), word);
            assert_eq!(
                serde_json::to_string(&role).expect("serialize"),
                format!("\"{word}\"")
            );
        }
    }
}
