//! session 管理：摘要列表、物理删除与自定义标题（重命名）。
//!
//! 基本读写（创建 / 追加 / 加载）见 crate 根；空壳 session
//! （无 user 消息）的过滤与条件清理见 [`SessionStore::delete_if_no_user_message`]。

use std::collections::HashMap;
use std::path::PathBuf;

use sqlx::Row as _;

use crate::{SessionError, SessionStore, to_u64};

/// session 摘要（`list_sessions` / `list_sessions_in` 返回）。
///
/// `id` 为内部标识（UUID v7），不对用户展示；用户可见的名称是
/// [`Self::title`]（自定义标题优先，缺省为首条 user 消息的首行摘要）。
/// 派生 serde（web 模式经 REST 列表给前端会话侧栏）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionSummary {
    /// session id（UUID v7；内部标识，不展示给用户）
    pub id: String,
    /// 会话标题：自定义标题（`rename_session`）优先，缺省为首条 user
    /// 消息的首行摘要（无消息时为 `None`，展示侧自行回退）
    pub title: Option<String>,
    /// 所属 work id
    pub work_id: String,
    /// 父 session id（子 agent 血缘；`None` = 主 session）
    pub parent_session_id: Option<String>,
    /// 所属 project id（经 work 派生）
    pub project_id: String,
    /// 所属 project 路径（session 操作的基准目录）
    pub project: PathBuf,
    /// 首条消息时间（Unix 毫秒；无消息时为 `None`）
    pub first_message_at: Option<u64>,
    /// 末条消息时间（Unix 毫秒；无消息时为 `None`）
    pub last_message_at: Option<u64>,
    /// 消息总数
    pub message_count: u64,
}

impl SessionStore {
    /// 列出全部 session 摘要（按末条消息时间降序）。
    ///
    /// 无 user 消息的 session 不出现（打开即退出、新建后未使用等空壳
    /// 不进入历史与统计口径；物理清理见 [`Self::delete_if_no_user_message`]）。
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let summaries = self.summarize(None).await?;
        tracing::debug!(count = summaries.len(), "listed sessions");
        Ok(summaries)
    }

    /// 列出指定 project 下的 session 摘要（排序同 [`Self::list_sessions`]）。
    pub async fn list_sessions_in(
        &self,
        project_id: &str,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        self.summarize(Some(project_id)).await
    }

    /// 列出指定 work 下的 session 摘要（主 session 与子 agent session；
    /// 排序同 [`Self::list_sessions`]）。
    pub async fn list_sessions_in_work(
        &self,
        work_id: &str,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let rows = self.summarize(None).await?;
        Ok(rows.into_iter().filter(|s| s.work_id == work_id).collect())
    }

    /// session 摘要查询内核：可选按 project 过滤；标题取自定义标题
    /// （`sessions.title`），缺失时经分组查询批量补齐派生标题。
    /// 只列出有 user 消息的 session（空壳 session 不是历史，见
    /// [`Self::list_sessions`]）。
    async fn summarize(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let rows = sqlx::query(
            "SELECT s.id, s.work_id, s.parent_session_id, p.id AS project_id,
                    p.path AS project_path, s.title,
                    s.first_message_at, s.last_message_at,
                    (SELECT COUNT(*) FROM entries e
                     WHERE e.session_id = s.id AND e.role <> 'compaction') AS message_count
             FROM sessions s
             JOIN works w ON w.id = s.work_id
             JOIN projects p ON p.id = w.project_id
             WHERE (?1 IS NULL OR w.project_id = ?1)
               AND EXISTS(SELECT 1 FROM entries e
                          WHERE e.session_id = s.id AND e.role = 'user')
             ORDER BY s.last_message_at IS NULL, s.last_message_at DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;

        let titles = self.fetch_titles().await?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row.get("id");
            let project_path: String = row.get("project_path");
            let custom: Option<String> = row.get("title");
            let first: Option<i64> = row.get("first_message_at");
            let last: Option<i64> = row.get("last_message_at");
            let count: i64 = row.get("message_count");
            summaries.push(SessionSummary {
                title: custom.or_else(|| titles.get(&id).cloned()),
                id,
                work_id: row.get("work_id"),
                parent_session_id: row.get("parent_session_id"),
                project_id: row.get("project_id"),
                project: PathBuf::from(project_path),
                first_message_at: first.map(to_u64),
                last_message_at: last.map(to_u64),
                message_count: to_u64(count),
            });
        }
        Ok(summaries)
    }

    /// 物理删除 session，返回是否实际删除（不存在返回 `Ok(false)`）。
    ///
    /// entries 与会话级 config 经外键 `ON DELETE CASCADE` 一并清除。
    /// 与 [`SessionStore::delete_if_no_user_message`] 的区别：本方法无条件
    /// 删除（用户显式删除历史会话），后者只清无 user 消息的空壳。
    pub async fn delete_session(&self, session_id: &str) -> Result<bool, SessionError> {
        let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        let deleted = result.rows_affected() > 0;
        if deleted {
            tracing::info!(session_id = %session_id, "session deleted");
        }
        Ok(deleted)
    }

    /// 设置（或清除）session 的自定义标题，返回生效的自定义标题。
    ///
    /// `title` 裁剪首尾空白后为空时清除自定义标题（回退派生标题——首条
    /// user 消息摘要），返回 `None`；否则写入并返回裁剪后的标题。
    /// session 不存在时报 [`SessionError::SessionNotFound`]。
    pub async fn rename_session(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<Option<String>, SessionError> {
        let title = title.trim();
        let title = (!title.is_empty()).then(|| title.to_string());
        let result = sqlx::query("UPDATE sessions SET title = ? WHERE id = ?")
            .bind(&title)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(SessionError::SessionNotFound(session_id.to_string()));
        }
        tracing::info!(session_id = %session_id, title = ?title, "session renamed");
        Ok(title)
    }

    /// session 的自定义标题（未设置或 session 不存在时为 `None`，展示侧
    /// 回退派生标题——首条 user 消息摘要）。
    pub async fn session_title_override(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, SessionError> {
        Ok(
            sqlx::query_scalar::<_, Option<String>>("SELECT title FROM sessions WHERE id = ?")
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await?
                .flatten(),
        )
    }

    /// 单个 session 的归属信息（快照与只读判定用）：所属 work id 与父
    /// session id（子 agent session 血缘，ADR-0044）；session 不存在时
    /// 为 `None`。
    pub async fn session_membership(
        &self,
        session_id: &str,
    ) -> Result<Option<(String, Option<String>)>, SessionError> {
        Ok(sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT work_id, parent_session_id FROM sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?)
    }
}

impl SessionStore {
    /// 各 session 的派生标题（首条 user 消息摘要）：一次分组查询取每个
    /// session 最早一条 user entry 的顶层块（text 拼接 / image 计数），
    /// 在内存计算摘要；无 user 消息或首条无正文的 session 不出现在结果中
    /// （派生标题为 `None`）。摘要口径与树列表的条目预览一致（lib.rs
    /// `entry_preview` 的 user 分支）。
    pub(crate) async fn fetch_titles(&self) -> Result<HashMap<String, String>, SessionError> {
        let rows = sqlx::query(
            "SELECT e.session_id, p.type, p.text FROM entries e
             JOIN (SELECT session_id, MIN(rowid) AS first_rowid FROM entries
                   WHERE role = 'user' GROUP BY session_id) f
             ON e.rowid = f.first_rowid
             JOIN parts p ON p.entry_id = e.id AND p.sub_seq = 0
             ORDER BY e.session_id, p.seq",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut titles = HashMap::new();
        let mut current: Option<(String, String, u64)> = None; // (session_id, text, images)
        for row in &rows {
            let session_id: String = row.get("session_id");
            if current.as_ref().is_none_or(|(id, ..)| *id != session_id) {
                flush_title(&mut titles, current.take());
                current = Some((session_id, String::new(), 0));
            }
            let Some((_, text, images)) = current.as_mut() else {
                continue;
            };
            match row.get::<&str, _>("type") {
                "text" => text.push_str(row.get::<Option<&str>, _>("text").unwrap_or_default()),
                "image" => *images += 1,
                _ => {}
            }
        }
        flush_title(&mut titles, current.take());
        Ok(titles)
    }
}

/// 汇总一个 session 的首条 user entry 摘要（文本首行 + 图片计数前缀），
/// 空摘要不入结果（口径同 lib.rs `entry_preview` 的 user 分支）。
fn flush_title(titles: &mut HashMap<String, String>, current: Option<(String, String, u64)>) {
    let Some((session_id, text, images)) = current else {
        return;
    };
    let title = if images == 0 {
        crate::first_line(&text)
    } else {
        format!("🖼 图片 ×{images} {}", crate::first_line(&text))
    }
    .trim()
    .to_string();
    if !title.is_empty() {
        titles.insert(session_id, title);
    }
}
