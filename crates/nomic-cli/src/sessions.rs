//! `nomic sessions` 子命令：历史 session 管理（当前仅 `list`）。

use anyhow::{Context as _, Result};
use nomic_session::SessionStore;
use time::macros::format_description;

/// 列出全部 session：id、最后更新时间、消息数与启动目录。
pub async fn list() -> Result<()> {
    let store = SessionStore::open_default()
        .await
        .context("打开 session 库失败")?;
    let sessions = store.list_sessions().await.context("列出 session 失败")?;
    if sessions.is_empty() {
        println!("没有历史 session。");
        return Ok(());
    }
    for summary in sessions {
        println!(
            "{}  {}  {:>4} 条消息  {}",
            summary.id,
            format_time(summary.last_message_at),
            summary.message_count,
            summary.cwd.display()
        );
    }
    Ok(())
}

/// Unix 毫秒时间戳 → `YYYY-MM-DD HH:MM`（本地时区，失败退回 UTC；无值显示 `-`）。
fn format_time(timestamp_ms: Option<u64>) -> String {
    const FORMAT: &[time::format_description::FormatItem<'static>] =
        format_description!("[year]-[month]-[day] [hour]:[minute]");
    let Some(ms) = timestamp_ms else {
        return "-".to_string();
    };
    let Ok(secs) = i64::try_from(ms / 1000) else {
        return "-".to_string();
    };
    let Ok(utc) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return "-".to_string();
    };
    let local = time::UtcOffset::current_local_offset().map_or(utc, |offset| utc.to_offset(offset));
    local.format(FORMAT).unwrap_or_else(|_| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_timestamp_shows_dash() {
        assert_eq!(format_time(None), "-");
    }

    #[test]
    fn formats_valid_timestamp() {
        // 2026-07-26T14:48:00Z 附近；本地时区只影响小时位，格式形状不变
        let text = format_time(Some(1_785_000_000_000));
        assert_eq!(text.len(), 16, "应为 YYYY-MM-DD HH:MM，实际：{text}");
        assert_eq!(&text[4..5], "-");
        assert_eq!(&text[13..14], ":");
    }

    #[test]
    fn out_of_range_timestamp_shows_dash() {
        assert_eq!(format_time(Some(u64::MAX)), "-");
    }
}
