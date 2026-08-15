//! 上下文压缩（compaction）的重建语义：合成摘要消息的构造与识别，以及
//! 「截尾 `kept_count` + 前置合成摘要」的有效上下文重建。
//!
//! 本 module 是重建语义的**唯一定义点**——in-memory 压缩（`nomic-core` 组装
//! 新历史）与 resume/branch 重放（`nomic-session` 沿分支路径重建）都经
//! [`apply_compaction`] 完成，保证两侧逐字节一致（见 `docs/adr/0005`）。
//!
//! ## 重建语义
//!
//! 压缩把较早的消息段替换为一条合成 user 摘要消息，保留近期 `kept_count`
//! 条消息原样：对当前有效上下文截尾到 `kept_count` 条，再前置
//! [`summary_message`] 构造的合成摘要消息。
//!
//! `kept_count` 是**相对计数**（压缩时保留的近期消息条数），代替 pi 的
//! `firstKeptEntryId`（绝对指针）。该递归语义对重复压缩天然成立：第二次
//! 压缩的 `kept_count` 相对第一次重建结果计数，摘要因此始终固定在
//! `messages[0]`。
//!
//! **分支路径**：branch 切换（`/tree` 选择分支起点）经
//! `nomic_session::SessionStore::load_branch` 沿所选 entry 的祖先路径重放，
//! 路径前缀即压缩发生时 agent 实际持有的上下文，因此 `kept_count` 相对计数
//! 在任意分支路径上依然精确（见 `docs/adr/0005` Amendments）。仅当未来支持
//! 跨分支移动 entry 时才需改回绝对指针（`first_kept_entry_id`）。

use crate::types::{Message, UserMessage, UserMessageContent};

/// 合成摘要消息的包装前缀（与 pi 的 `COMPACTION_SUMMARY_PREFIX` 一致）。
///
/// 该前缀同时是识别标记：session 重建、二次压缩提取 previous summary、
/// 交互端压缩渲染都靠它判定一条 user 消息是否为压缩摘要。
pub const SUMMARY_PREFIX: &str = "The conversation history before this point was compacted into the following summary:\n<summary>\n";

/// 合成摘要消息的包装后缀（与 pi 一致）。
pub const SUMMARY_SUFFIX: &str = "\n</summary>";

/// 构造压缩摘要的合成 user 消息（上下文压缩把较早消息段替换为该消息）。
pub fn summary_message(summary: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(format!("{SUMMARY_PREFIX}{summary}{SUMMARY_SUFFIX}")),
        timestamp,
    })
}

/// 判定一条消息是否为压缩摘要（包装前缀识别）。
pub fn is_summary_message(message: &Message) -> bool {
    extract_summary(message).is_some()
}

/// 从合成摘要消息中提取摘要正文（非摘要消息返回 `None`）。
pub fn extract_summary(message: &Message) -> Option<&str> {
    let Message::User(user) = message else {
        return None;
    };
    let UserMessageContent::Text(text) = &user.content else {
        return None;
    };
    text.strip_prefix(SUMMARY_PREFIX)
        .and_then(|rest| rest.strip_suffix(SUMMARY_SUFFIX))
}

/// 应用一次压缩，重建有效上下文（语义见 module 文档）。
///
/// 把 `history` 截尾到最近 `kept_count` 条，前置 `summary` 的合成摘要消息
/// （timestamp 由调用方给定：in-memory 压缩取当前时间，resume 重放取压缩
/// 条目的落库时间，两侧各取一次、不重复合成）。
///
/// `kept_count` 超出 `history` 长度时按全部保留处理（重放路径的防御性钳制，
/// in-memory 压缩的计数恒为精确值）。
#[must_use]
pub fn apply_compaction(
    history: &[Message],
    summary: &str,
    kept_count: u64,
    timestamp: u64,
) -> Vec<Message> {
    let keep = usize::try_from(kept_count)
        .unwrap_or(usize::MAX)
        .min(history.len());
    let mut effective = Vec::with_capacity(keep + 1);
    effective.push(summary_message(summary, timestamp));
    effective.extend_from_slice(&history[history.len() - keep..]);
    effective
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_string()),
            timestamp: 0,
        })
    }

    fn text_of(message: &Message) -> &str {
        let Message::User(user) = message else {
            panic!("expected user message");
        };
        let UserMessageContent::Text(text) = &user.content else {
            panic!("expected text content");
        };
        text
    }

    #[test]
    fn summary_message_roundtrips_through_marker() {
        let message = summary_message("## Goal\ndo stuff", 0);
        assert!(is_summary_message(&message));
        assert_eq!(extract_summary(&message), Some("## Goal\ndo stuff"));
        // 仅含前缀片段的普通 user 消息不应误判
        assert!(!is_summary_message(&user(
            "The conversation history before this point"
        )));
    }

    #[test]
    fn apply_compaction_keeps_exact_tail() {
        let history: Vec<Message> = (0..5).map(|i| user(&format!("m{i}"))).collect();
        let effective = apply_compaction(&history, "summary-1", 2, 42);
        assert_eq!(effective.len(), 3);
        assert_eq!(extract_summary(&effective[0]), Some("summary-1"));
        assert_eq!(text_of(&effective[1]), "m3");
        assert_eq!(text_of(&effective[2]), "m4");
    }

    #[test]
    fn apply_compaction_clamps_kept_count_beyond_history_len() {
        let history = vec![user("only")];
        let effective = apply_compaction(&history, "s", u64::MAX, 0);
        assert_eq!(effective.len(), 2);
        assert_eq!(text_of(&effective[1]), "only");
    }

    #[test]
    fn apply_compaction_zero_kept_yields_summary_only() {
        let history = vec![user("a"), user("b")];
        let effective = apply_compaction(&history, "s", 0, 0);
        assert_eq!(effective.len(), 1);
        assert_eq!(extract_summary(&effective[0]), Some("s"));
    }

    #[test]
    fn apply_compaction_recursion_keeps_summary_at_head() {
        let history: Vec<Message> = (0..6).map(|i| user(&format!("m{i}"))).collect();
        let once = apply_compaction(&history, "summary-1", 3, 1);
        // 第二次压缩的 kept_count 相对第一次重建结果计数
        let twice = apply_compaction(&once, "summary-2", 2, 2);
        assert_eq!(twice.len(), 3);
        assert_eq!(extract_summary(&twice[0]), Some("summary-2"));
        assert_eq!(text_of(&twice[1]), "m4");
        assert_eq!(text_of(&twice[2]), "m5");
    }
}
