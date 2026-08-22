//! workspace：文件系统路径的一等实体。
//!
//! 一个 workspace 对应一个规范化路径（`workspaces.path` 全局唯一），session
//! 创建时绑定 workspace（`sessions.workspace_id`），其所有操作以 workspace
//! 路径为基准（工具相对路径解析、mention 展开、`--continue` 匹配等）。
//! `last_active_at` 在 session 创建与条目追加时推进，供 workspace 列表排序。

use std::path::{Path, PathBuf};

use nomic_ai::now_millis;
use sqlx::Row as _;

use crate::{SessionError, SessionStore, to_i64, to_u64};

/// workspace 实体（`get_or_create_workspace` 等返回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// workspace id（UUID v7；迁移历史行可能为随机 hex）
    pub id: String,
    /// 规范化路径（创建时 canonicalize，符号链接已解析）
    pub path: PathBuf,
}

/// workspace 摘要（`list_workspaces` 返回，列表展示用）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceSummary {
    /// workspace id
    pub id: String,
    /// 规范化路径
    pub path: PathBuf,
    /// 名下有 user 消息的 session 总数（空壳 session 不计入，口径同
    /// `list_sessions`）
    pub session_count: u64,
    /// 最近活跃时间（Unix 毫秒；从未有 session 活动时为 `None`）
    pub last_active_at: Option<u64>,
}

impl SessionStore {
    /// 按路径取 workspace，不存在则登记（路径先规范化：优先 canonicalize，
    /// 不存在时退回原始路径）。
    ///
    /// 「查或插」分两步而非单事务：并发插入撞 `path` UNIQUE 时回退为读取，
    /// 无需为幂等登记引入事务开销。
    pub async fn get_or_create_workspace(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Workspace, SessionError> {
        let path = normalize_path(path.as_ref());
        let text = path.to_string_lossy().into_owned();
        if let Some(workspace) = self.workspace_by_path(&text).await? {
            tracing::debug!(workspace_id = %workspace.id, path = %text, "workspace: existing found");
            return Ok(workspace);
        }
        let id = uuid::Uuid::now_v7().to_string();
        tracing::info!(workspace_id = %id, path = %text, "workspace: creating new");
        sqlx::query("INSERT OR IGNORE INTO workspaces (id, path, created_at) VALUES (?, ?, ?)")
            .bind(&id)
            .bind(&text)
            .bind(to_i64(now_millis()))
            .execute(&self.pool)
            .await?;
        // 并发撞唯一约束时上面是 no-op，统一以读取为准
        self.workspace_by_path(&text)
            .await?
            .ok_or_else(|| SessionError::WorkspaceNotFound(text))
    }

    /// 在指定 workspace 下创建 session，返回 session id（UUID v7 字符串）。
    ///
    /// 同事务内推进 `workspaces.last_active_at`；`workspace_id` 不存在时由
    /// 外键约束拒绝。
    pub async fn create_session_in(&self, workspace_id: &str) -> Result<String, SessionError> {
        let id = uuid::Uuid::now_v7().to_string();
        tracing::debug!(session_id = %id, workspace_id = %workspace_id, "creating session in workspace");
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO sessions (id, workspace_id) VALUES (?, ?)")
            .bind(&id)
            .bind(workspace_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE workspaces SET last_active_at = ? WHERE id = ?")
            .bind(to_i64(now_millis()))
            .bind(workspace_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// 按 id 取 workspace。
    pub async fn workspace(&self, workspace_id: &str) -> Result<Option<Workspace>, SessionError> {
        let row = sqlx::query("SELECT id, path FROM workspaces WHERE id = ?")
            .bind(workspace_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| Workspace {
            id: row.get("id"),
            path: PathBuf::from(row.get::<String, _>("path")),
        }))
    }

    /// 按规范化路径取 workspace（未登记为 `None`）。
    pub async fn workspace_by_path(&self, path: &str) -> Result<Option<Workspace>, SessionError> {
        let row = sqlx::query("SELECT id, path FROM workspaces WHERE path = ?")
            .bind(path)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| Workspace {
            id: row.get("id"),
            path: PathBuf::from(row.get::<String, _>("path")),
        }))
    }

    /// session 所属的 workspace。
    pub async fn workspace_of_session(
        &self,
        session_id: &str,
    ) -> Result<Option<Workspace>, SessionError> {
        let row = sqlx::query(
            "SELECT w.id, w.path FROM workspaces w
             JOIN sessions s ON s.workspace_id = w.id WHERE s.id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| Workspace {
            id: row.get("id"),
            path: PathBuf::from(row.get::<String, _>("path")),
        }))
    }

    /// session 的 workspace 路径（严格归属：resume 后以此为操作基准）。
    pub async fn session_workspace_path(&self, session_id: &str) -> Result<PathBuf, SessionError> {
        let path: String = sqlx::query_scalar(
            "SELECT w.path FROM workspaces w
             JOIN sessions s ON s.workspace_id = w.id WHERE s.id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| SessionError::SessionNotFound(session_id.to_string()))?;
        Ok(PathBuf::from(path))
    }

    /// 列出全部 workspace 摘要（按最近活跃降序，从未活跃的排最后）。
    ///
    /// `session_count` 只统计有 user 消息的 session（与 `list_sessions`
    /// 同一口径：空壳 session 不进统计）。
    pub async fn list_workspaces(&self) -> Result<Vec<WorkspaceSummary>, SessionError> {
        let rows = sqlx::query(
            "SELECT w.id, w.path, w.last_active_at,
                    (SELECT COUNT(*) FROM sessions s WHERE s.workspace_id = w.id
                       AND EXISTS(SELECT 1 FROM entries e
                                  WHERE e.session_id = s.id
                                    AND e.kind = 'message' AND e.role = 'user')
                    ) AS session_count
             FROM workspaces w
             ORDER BY w.last_active_at IS NULL, w.last_active_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let last: Option<i64> = row.get("last_active_at");
                let count: i64 = row.get("session_count");
                WorkspaceSummary {
                    id: row.get("id"),
                    path: PathBuf::from(row.get::<String, _>("path")),
                    session_count: to_u64(count),
                    last_active_at: last.map(to_u64),
                }
            })
            .collect())
    }

    /// 条目追加时推进所属 workspace 的活跃时间（`append_entry` 事务内调用）。
    pub(crate) async fn touch_workspace(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        session_id: &str,
        timestamp: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE workspaces SET last_active_at = ?
             WHERE id = (SELECT workspace_id FROM sessions WHERE id = ?)",
        )
        .bind(to_i64(timestamp))
        .bind(session_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }
}

/// 路径规范化：优先 canonicalize（解析符号链接），路径不存在时退回原始路径。
fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
