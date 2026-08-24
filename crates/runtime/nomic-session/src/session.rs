//! session 管理：物理删除与自定义标题（重命名）。
//!
//! 基本读写（创建 / 追加 / 加载 / 摘要列表）见 crate 根；空壳 session
//! （无 user 消息）的过滤与条件清理见 [`SessionStore::delete_if_no_user_message`]。

use crate::{SessionError, SessionStore};

impl SessionStore {
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
}
