//! `parts` 表行 ↔ [`Part`] 内容块的编解码（schema 见 migration 0013）。
//!
//! 主键 `(entry_id, seq, sub_seq)`：`seq` 为顶层块序；tool_result 的嵌套
//! 内容块（text/image）以 `sub_seq > 0` 挂在其所属块（`sub_seq = 0`）下。
//! 列复用约定：`text` 列承载 text/thinking 的文本，`signature` 列承载
//! text/thinking/tool_call 三种签名。解码遇未知类型或必填字段缺失返回
//! `None`（数据损坏），由调用方决定降级策略（预览占位 / 加载报错）。

use std::collections::HashMap;

use nomic_ai::{
    CompactionRecord, ImageContent, Part, TextContent, ThinkingContent, ToolCall, ToolResultPart,
};
use sqlx::sqlite::{Sqlite, SqliteRow};
use sqlx::{Row as _, Transaction};

use crate::to_u64;

/// 把一个 entry 的全部内容块写入 parts 表（在调用方事务内执行）。
pub async fn insert_parts(
    tx: &mut Transaction<'_, Sqlite>,
    entry_id: &str,
    parts: &[Part],
) -> Result<(), sqlx::Error> {
    for (seq, part) in parts.iter().enumerate() {
        let seq = crate::to_i64(seq as u64);
        insert_part(tx, entry_id, seq, 0, part).await?;
        if let Part::ToolResult(result) = part {
            for (sub, content) in result.content.iter().enumerate() {
                let sub_seq = crate::to_i64(sub as u64 + 1);
                insert_part(tx, entry_id, seq, sub_seq, content).await?;
            }
        }
    }
    Ok(())
}

/// 单行写入：列按 part 类型取对应字段，其余列为 NULL/默认值。
async fn insert_part(
    tx: &mut Transaction<'_, Sqlite>,
    entry_id: &str,
    seq: i64,
    sub_seq: i64,
    part: &Part,
) -> Result<(), sqlx::Error> {
    let mut row = PartRow::new(part_kind(part));
    match part {
        Part::Text(text) => {
            row.text = Some(&text.text);
            row.signature = text.text_signature.as_deref();
        }
        Part::Thinking(thinking) => {
            row.text = Some(&thinking.thinking);
            row.signature = thinking.thinking_signature.as_deref();
            row.redacted = thinking.redacted;
        }
        Part::Image(image) => {
            row.data = Some(&image.data);
            row.mime_type = Some(&image.mime_type);
        }
        Part::ToolCall(call) => {
            row.tool_call_id = Some(&call.id);
            row.tool_name = Some(&call.name);
            row.arguments = Some(
                serde_json::to_string(&call.arguments)
                    .map_err(|e| sqlx::Error::Encode(Box::new(e)))?,
            );
            row.signature = call.thought_signature.as_deref();
        }
        Part::ToolResult(result) => {
            row.tool_call_id = Some(&result.tool_call_id);
            row.tool_name = Some(&result.tool_name);
            row.details = result
                .details
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
            row.is_error = result.is_error;
        }
        Part::Compaction(record) => {
            row.summary = Some(&record.summary);
            row.kept_count = Some(crate::to_i64(record.kept_count));
            row.tokens_before = Some(crate::to_i64(record.tokens_before));
        }
    }
    sqlx::query(
        "INSERT INTO parts (entry_id, seq, sub_seq, type, text, signature, redacted,
                            data, mime_type, tool_call_id, tool_name, arguments, details,
                            is_error, summary, kept_count, tokens_before)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(entry_id)
    .bind(seq)
    .bind(sub_seq)
    .bind(row.kind)
    .bind(row.text)
    .bind(row.signature)
    .bind(row.redacted)
    .bind(row.data)
    .bind(row.mime_type)
    .bind(row.tool_call_id)
    .bind(row.tool_name)
    .bind(row.arguments)
    .bind(row.details)
    .bind(row.is_error)
    .bind(row.summary)
    .bind(row.kept_count)
    .bind(row.tokens_before)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 待写入的一行 parts（按 part 类型填对应列）。
struct PartRow<'a> {
    kind: &'static str,
    text: Option<&'a str>,
    signature: Option<&'a str>,
    redacted: bool,
    data: Option<&'a str>,
    mime_type: Option<&'a str>,
    tool_call_id: Option<&'a str>,
    tool_name: Option<&'a str>,
    arguments: Option<String>,
    details: Option<String>,
    is_error: bool,
    summary: Option<&'a str>,
    kept_count: Option<i64>,
    tokens_before: Option<i64>,
}

impl PartRow<'_> {
    const fn new(kind: &'static str) -> Self {
        Self {
            kind,
            text: None,
            signature: None,
            redacted: false,
            data: None,
            mime_type: None,
            tool_call_id: None,
            tool_name: None,
            arguments: None,
            details: None,
            is_error: false,
            summary: None,
            kept_count: None,
            tokens_before: None,
        }
    }
}

const fn part_kind(part: &Part) -> &'static str {
    match part {
        Part::Text(_) => "text",
        Part::Thinking(_) => "thinking",
        Part::Image(_) => "image",
        Part::ToolCall(_) => "tool_call",
        Part::ToolResult(_) => "tool_result",
        Part::Compaction(_) => "compaction",
    }
}

/// 读取 session 的全部 parts 行并按 entry 组装为有序块列表；
/// 任一行损坏（未知类型、必填字段缺失、孤儿嵌套块）时该 entry 映射为
/// `None`（由调用方按数据损坏降级）。
pub async fn load_parts(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: &str,
) -> Result<HashMap<String, Option<Vec<Part>>>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT p.entry_id, p.seq, p.sub_seq, p.type, p.text, p.signature, p.redacted,
                p.data, p.mime_type, p.tool_call_id, p.tool_name, p.arguments, p.details,
                p.is_error, p.summary, p.kept_count, p.tokens_before
         FROM parts p JOIN entries e ON e.id = p.entry_id
         WHERE e.session_id = ? ORDER BY p.entry_id, p.seq, p.sub_seq",
    )
    .bind(session_id)
    .fetch_all(&mut **tx)
    .await?;

    // 按 entry 分组；每组先收顶层块（sub_seq=0）再归位嵌套块
    let mut grouped: HashMap<String, Vec<&SqliteRow>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for row in &rows {
        let entry_id: String = row.get("entry_id");
        grouped.entry(entry_id.clone()).or_default().push(row);
        if order.last() != Some(&entry_id) {
            order.push(entry_id);
        }
    }

    let mut result = HashMap::with_capacity(grouped.len());
    for entry_id in order {
        let rows = &grouped[&entry_id];
        result.insert(entry_id, assemble_entry_parts(rows));
    }
    Ok(result)
}

/// 单个 entry 的 parts 行 → 有序块列表（任一行损坏则整体 `None`）。
fn assemble_entry_parts(rows: &[&SqliteRow]) -> Option<Vec<Part>> {
    let mut parts: Vec<Part> = Vec::new();
    for row in rows {
        let sub_seq: i64 = row.get("sub_seq");
        if sub_seq == 0 {
            parts.push(decode_part(row)?);
            continue;
        }
        // 嵌套块：归位到所属 tool_result 顶层块的内容末尾
        let seq: i64 = row.get("seq");
        let parent = parts.get_mut(usize::try_from(seq).ok()?);
        let Some(Part::ToolResult(result)) = parent else {
            return None; // 孤儿嵌套块（父块缺失或非 tool_result）
        };
        result.content.push(decode_part(row)?);
    }
    Some(parts)
}

/// 一行 parts → Part；未知类型或必填字段缺失返回 `None`（数据损坏）。
fn decode_part(row: &SqliteRow) -> Option<Part> {
    let kind: &str = row.get("type");
    let text: Option<String> = row.get("text");
    let signature: Option<String> = row.get("signature");
    match kind {
        "text" => Some(Part::Text(TextContent {
            text: text?,
            text_signature: signature,
        })),
        "thinking" => Some(Part::Thinking(ThinkingContent {
            thinking: text?,
            thinking_signature: signature,
            redacted: row.get::<i64, _>("redacted") != 0,
        })),
        "image" => Some(Part::Image(ImageContent {
            data: row.get::<Option<String>, _>("data")?,
            mime_type: row.get::<Option<String>, _>("mime_type")?,
        })),
        "tool_call" => Some(Part::ToolCall(ToolCall {
            id: row.get::<Option<String>, _>("tool_call_id")?,
            name: row.get::<Option<String>, _>("tool_name")?,
            arguments: row
                .get::<Option<String>, _>("arguments")
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .ok()?
                .unwrap_or(serde_json::Value::Null),
            thought_signature: signature,
        })),
        "tool_result" => Some(Part::ToolResult(ToolResultPart {
            tool_call_id: row.get::<Option<String>, _>("tool_call_id")?,
            tool_name: row.get::<Option<String>, _>("tool_name")?,
            content: Vec::new(), // 嵌套块由 assemble_entry_parts 归位
            details: row
                .get::<Option<String>, _>("details")
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .ok()?,
            is_error: row.get::<i64, _>("is_error") != 0,
        })),
        "compaction" => Some(Part::Compaction(CompactionRecord {
            summary: row.get::<Option<String>, _>("summary")?,
            kept_count: to_u64(row.get::<Option<i64>, _>("kept_count")?),
            tokens_before: to_u64(row.get::<Option<i64>, _>("tokens_before")?),
        })),
        _ => None,
    }
}
