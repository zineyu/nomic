//! work：一次任务的完整协作过程（一等实体，ADR-0044）。
//!
//! 归属链 project 1—N work 1—N session（均 NOT NULL）：session 经
//! `sessions.work_id` 归属 work，work 经 `works.project_id` 归属 project；
//! session 的 project 由该链派生（严格归属口径不变）。
//!
//! work 占据原 session 的用户入口位置：创建对话即创建 work（连带主
//! session），列表/恢复/删除均以 work 为单位。多 agent 协作时子 agent
//! 落库为同一 work 下的 session（`sessions.parent_session_id` 记血缘，
//! NULL = 主 session）。删除 work 级联删除名下全部 session（entries 与
//! 会话级 config 经外键 `ON DELETE CASCADE` 清除）。
//!
//! 无 user 消息的 work（打开即退出等空壳）不进入列表口径，并在 session
//! 结束点随 [`SessionStore::delete_if_no_user_message`] 物理清除。

use std::path::PathBuf;

use nomic_ai::now_millis;
use sqlx::Row as _;

use crate::{SessionError, SessionStore, to_i64, to_u64};

/// work 实体（[`SessionStore::work`] / [`SessionStore::work_of_session`] 返回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Work {
    /// work id（UUID v7；迁移回填行为 `'w-' + session id`）
    pub id: String,
    /// 所属 project id
    pub project_id: String,
    /// 自定义标题（`rename_work`）；`None` = 派生（主 session 标题）
    pub title: Option<String>,
    /// 创建时间（Unix 毫秒）
    pub created_at: u64,
    /// 最近活跃（session 创建 / 条目追加时推进）
    pub last_active_at: Option<u64>,
}

/// work 摘要（`list_works` / `list_works_in` 返回，列表展示用）。
///
/// `title` 已解析：work 自定义标题优先，缺省回落主 session 标题（自定义
/// 或首条 user 消息派生）。派生 serde（web 模式经 WS 列表给前端侧栏）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkSummary {
    /// work id
    pub id: String,
    /// 主 session id（进入 work 默认打开它）
    pub main_session_id: String,
    /// 标题（自定义优先，缺省派生；无消息时为 `None`，展示侧自行回退）
    pub title: Option<String>,
    /// 所属 project id
    pub project_id: String,
    /// 所属 project 路径（work 操作的基准目录）
    pub project: PathBuf,
    /// 名下 session 总数（含子 agent session）
    pub session_count: u64,
    /// 首条消息时间（Unix 毫秒；跨 session 取最早）
    pub first_message_at: Option<u64>,
    /// 末条消息时间（Unix 毫秒；跨 session 取最晚）
    pub last_message_at: Option<u64>,
    /// 名下全部 session 的消息总数
    pub message_count: u64,
}

/// `create_work` 的返回：work id 与连带创建的主 session id。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkCreated {
    /// work id
    pub work_id: String,
    /// 主 session id
    pub session_id: String,
}

impl SessionStore {
    /// 在指定路径的 project 下创建 work（project 不存在则登记），连带
    /// 创建主 session，返回两者 id。
    pub async fn create_work(
        &self,
        project: impl AsRef<std::path::Path>,
    ) -> Result<WorkCreated, SessionError> {
        let project = self.get_or_create_project(project).await?;
        let created = self.create_work_in(&project.id).await?;
        tracing::info!(work_id = %created.work_id, session_id = %created.session_id, project_id = %project.id, "work created");
        Ok(created)
    }

    /// 在指定 project 下创建 work 与主 session（同事务）。
    pub async fn create_work_in(&self, project_id: &str) -> Result<WorkCreated, SessionError> {
        let work_id = uuid::Uuid::now_v7().to_string();
        let session_id = uuid::Uuid::now_v7().to_string();
        let now = to_i64(now_millis());
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO works (id, project_id, created_at) VALUES (?, ?, ?)")
            .bind(&work_id)
            .bind(project_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO sessions (id, work_id) VALUES (?, ?)")
            .bind(&session_id)
            .bind(&work_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE projects SET last_active_at = ? WHERE id = ?")
            .bind(now)
            .bind(project_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(WorkCreated {
            work_id,
            session_id,
        })
    }

    /// 在既有 work 下创建 session：`parent_session_id` 为 `None` 时是
    /// 额外主会话入口（非常规路径），`Some` 时是子 agent session
    /// （记父血缘；多 agent 协作落库，见 ADR-0031/0044）。同事务内推进
    /// `works.last_active_at`；`work_id` 不存在时由外键约束拒绝。
    pub async fn create_session_in_work(
        &self,
        work_id: &str,
        parent_session_id: Option<&str>,
    ) -> Result<String, SessionError> {
        let id = uuid::Uuid::now_v7().to_string();
        tracing::debug!(session_id = %id, work_id, parent_session_id, "creating session in work");
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO sessions (id, work_id, parent_session_id) VALUES (?, ?, ?)")
            .bind(&id)
            .bind(work_id)
            .bind(parent_session_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE works SET last_active_at = ? WHERE id = ?")
            .bind(to_i64(now_millis()))
            .bind(work_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// 按 id 取 work。
    pub async fn work(&self, work_id: &str) -> Result<Option<Work>, SessionError> {
        let row = sqlx::query(
            "SELECT id, project_id, title, created_at, last_active_at FROM works WHERE id = ?",
        )
        .bind(work_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_work))
    }

    /// session 所属的 work。
    pub async fn work_of_session(&self, session_id: &str) -> Result<Option<Work>, SessionError> {
        let row = sqlx::query(
            "SELECT w.id, w.project_id, w.title, w.created_at, w.last_active_at
             FROM works w JOIN sessions s ON s.work_id = w.id WHERE s.id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(row_to_work))
    }

    /// work 的主 session id（`parent_session_id IS NULL` 中最早创建的；
    /// work 无任何 session 时为 `None`——仅并发破坏下可能出现）。
    pub async fn main_session_of_work(
        &self,
        work_id: &str,
    ) -> Result<Option<String>, SessionError> {
        let id = sqlx::query_scalar::<_, String>(
            "SELECT id FROM sessions WHERE work_id = ? AND parent_session_id IS NULL
             ORDER BY rowid LIMIT 1",
        )
        .bind(work_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(id)
    }

    /// 列出全部 work 摘要（按末条消息时间降序；只列出含 user 消息的
    /// work，空壳口径同 `list_sessions`）。
    pub async fn list_works(&self) -> Result<Vec<WorkSummary>, SessionError> {
        self.summarize_works(None).await
    }

    /// 列出指定 project 下的 work 摘要（排序同 [`Self::list_works`]）。
    pub async fn list_works_in(&self, project_id: &str) -> Result<Vec<WorkSummary>, SessionError> {
        self.summarize_works(Some(project_id)).await
    }

    /// work 摘要查询内核：可选按 project 过滤；标题优先 work 自定义
    /// （`works.title`），缺失时回落主 session 标题（自定义或派生）。
    ///
    /// 集合式聚合：session 计数/时间窗、主 session、消息计数、空壳过滤
    /// 各自单次扫描（GROUP BY / DISTINCT），再按 work_id 一次性 JOIN——
    /// 避免逐行相关子查询在 entries 大表上的 O(works × sessions) 放大。
    async fn summarize_works(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<WorkSummary>, SessionError> {
        let rows = sqlx::query(
            "WITH per_work AS (
                 SELECT work_id,
                        COUNT(*) AS session_count,
                        MIN(first_message_at) AS first_message_at,
                        MAX(last_message_at) AS last_message_at
                 FROM sessions
                 GROUP BY work_id
             ),
             main_session AS (
                 SELECT work_id, id, title
                 FROM (SELECT work_id, id, title,
                              ROW_NUMBER() OVER (PARTITION BY work_id ORDER BY rowid) AS rn
                       FROM sessions
                       WHERE parent_session_id IS NULL)
                 WHERE rn = 1
             ),
             message_counts AS (
                 SELECT s.work_id, COUNT(*) AS message_count
                 FROM entries e JOIN sessions s ON s.id = e.session_id
                 WHERE e.kind = 'message'
                 GROUP BY s.work_id
             ),
             works_with_user AS (
                 SELECT DISTINCT s.work_id
                 FROM entries e JOIN sessions s ON s.id = e.session_id
                 WHERE e.kind = 'message' AND e.role = 'user'
             )
             SELECT w.id, w.project_id, p.path AS project_path, w.title,
                    m.id AS main_session_id,
                    COALESCE(a.session_count, 0) AS session_count,
                    a.first_message_at,
                    a.last_message_at,
                    COALESCE(mc.message_count, 0) AS message_count,
                    m.title AS main_title
             FROM works w
             JOIN projects p ON p.id = w.project_id
             JOIN works_with_user hu ON hu.work_id = w.id
             LEFT JOIN per_work a ON a.work_id = w.id
             LEFT JOIN main_session m ON m.work_id = w.id
             LEFT JOIN message_counts mc ON mc.work_id = w.id
             WHERE (?1 IS NULL OR w.project_id = ?1)
             ORDER BY a.last_message_at IS NULL, a.last_message_at DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;

        let titles = self.fetch_titles().await?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in &rows {
            let main_session_id: Option<String> = row.get("main_session_id");
            let work_title: Option<String> = row.get("title");
            let main_custom: Option<String> = row.get("main_title");
            let title = work_title.or(main_custom).or_else(|| {
                main_session_id
                    .as_ref()
                    .and_then(|id| titles.get(id).cloned())
            });
            let first: Option<i64> = row.get("first_message_at");
            let last: Option<i64> = row.get("last_message_at");
            let session_count: i64 = row.get("session_count");
            let message_count: i64 = row.get("message_count");
            let project_path: String = row.get("project_path");
            summaries.push(WorkSummary {
                id: row.get("id"),
                main_session_id: main_session_id.unwrap_or_default(),
                title,
                project_id: row.get("project_id"),
                project: PathBuf::from(project_path),
                session_count: to_u64(session_count),
                first_message_at: first.map(to_u64),
                last_message_at: last.map(to_u64),
                message_count: to_u64(message_count),
            });
        }
        Ok(summaries)
    }

    /// 重命名 work：返回生效的自定义标题（`None` = 已清除，回退派生）；
    /// work 不存在时报 [`SessionError::WorkNotFound`]。
    pub async fn rename_work(
        &self,
        work_id: &str,
        title: &str,
    ) -> Result<Option<String>, SessionError> {
        let title = title.trim();
        let title = (!title.is_empty()).then(|| title.to_string());
        let result = sqlx::query("UPDATE works SET title = ? WHERE id = ?")
            .bind(&title)
            .bind(work_id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(SessionError::WorkNotFound(work_id.to_string()));
        }
        tracing::info!(work_id, title = ?title, "work renamed");
        Ok(title)
    }

    /// 删除 work：级联删除名下全部 session（entries 与会话级 config 经
    /// 外键 `ON DELETE CASCADE` 清除），返回名下被删除的 session id 列表；
    /// work 不存在时返回 [`SessionError::WorkNotFound`]。
    pub async fn delete_work(&self, work_id: &str) -> Result<Vec<String>, SessionError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let session_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM sessions WHERE work_id = ?")
                .bind(work_id)
                .fetch_all(&mut *tx)
                .await?;
        let result = sqlx::query("DELETE FROM works WHERE id = ?")
            .bind(work_id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            return Err(SessionError::WorkNotFound(work_id.to_string()));
        }
        tx.commit().await?;
        tracing::info!(work_id, sessions = session_ids.len(), "work deleted");
        Ok(session_ids)
    }

    /// 条目追加时推进所属 work 的活跃时间（`append_entry` 事务内调用，
    /// 与 [`SessionStore::touch_project`] 同事务）。
    pub(crate) async fn touch_work(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        session_id: &str,
        timestamp: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE works SET last_active_at = ?
             WHERE id = (SELECT work_id FROM sessions WHERE id = ?)",
        )
        .bind(to_i64(timestamp))
        .bind(session_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// 清除无任何 user 消息的空壳 work（名下 session 全为空壳时级联
    /// 删除）；[`SessionStore::delete_if_no_user_message`] 的连带清理。
    pub(crate) async fn delete_work_if_no_user_message(
        &self,
        work_id: &str,
    ) -> Result<bool, SessionError> {
        let result = sqlx::query(
            "DELETE FROM works WHERE id = ?
               AND NOT EXISTS(SELECT 1 FROM sessions s
                              WHERE s.work_id = works.id
                                AND EXISTS(SELECT 1 FROM entries e
                                           WHERE e.session_id = s.id
                                             AND e.kind = 'message' AND e.role = 'user'))",
        )
        .bind(work_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}

/// works 行 → [`Work`]。
fn row_to_work(row: &sqlx::sqlite::SqliteRow) -> Work {
    let created: i64 = row.get("created_at");
    let last: Option<i64> = row.get("last_active_at");
    Work {
        id: row.get("id"),
        project_id: row.get("project_id"),
        title: row.get("title"),
        created_at: to_u64(created),
        last_active_at: last.map(to_u64),
    }
}
