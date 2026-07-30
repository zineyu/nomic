//! nomic-session：SQLite session 持久化（M2，替代 pi 的 JSONL session 文件）。
//!
//! - 每个 session 一个唯一 id（UUID v7，时间有序），`sessions` 表记录
//!   首/末消息时间与启动位置（cwd）
//! - 消息存 `entries` 表，按 `parent_id` 组织为**树**（为 ADR-0001 的
//!   branching 目标预留；顺序会话是树的特例）
//! - 全局单库，默认位于 XDG data dir：`$XDG_DATA_HOME/nomic/sessions.db`
//!
//! 消息 payload 原样存 [`Message`] 的 serde JSON；`role`/`timestamp` 为提取列，
//! 供查询与维护 session 时间字段。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nomic_ai::Message;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row as _, SqlitePool};

/// 内嵌迁移（`crates/nomic-session/migrations/`）。
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// session 存储层的错误。
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// SQLite 运行时错误
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    /// 迁移执行失败
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// 文件系统错误（创建目录、解析默认路径等）
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// session id 不存在
    #[error("session not found: {0}")]
    SessionNotFound(String),
    /// entry id 不存在（或不属于目标 session）
    #[error("entry not found: {0}")]
    EntryNotFound(String),
    /// 库中 payload 不是合法的 [`Message`] JSON（数据损坏）
    #[error("message payload corrupted: {0}")]
    Corrupt(#[from] serde_json::Error),
}

/// session 摘要（`list_sessions` 返回）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// session id（UUID v7）
    pub id: String,
    /// 启动位置（工作目录）
    pub cwd: PathBuf,
    /// 首条消息时间（Unix 毫秒；无消息时为 `None`）
    pub first_message_at: Option<u64>,
    /// 末条消息时间（Unix 毫秒；无消息时为 `None`）
    pub last_message_at: Option<u64>,
    /// 消息总数
    pub message_count: u64,
}

/// SQLite session 存储。
///
/// 所有方法均为异步；写入路径同事务维护 `sessions` 的时间字段。
#[derive(Debug, Clone)]
pub struct SessionStore {
    pool: SqlitePool,
}

impl SessionStore {
    /// 打开（或创建）指定路径的库并执行迁移。
    ///
    /// 自动创建父目录；连接开启 WAL 与外键约束。
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            // 相对路径 "sessions.db" 的 parent 是空串，create_dir_all("") 会失败
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(options).await?;
        Self::migrate(pool).await
    }

    /// 打开默认路径（XDG data dir）的库，见 [`default_db_path`]。
    pub async fn open_default() -> Result<Self, SessionError> {
        Self::open(default_db_path()?).await
    }

    /// 打开内存库（测试与嵌入式场景）。
    ///
    /// 连接数固定为 1：内存库按连接隔离，多连接会互相看不见。
    pub async fn in_memory() -> Result<Self, SessionError> {
        let options = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        Self::migrate(pool).await
    }

    async fn migrate(pool: SqlitePool) -> Result<Self, SessionError> {
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    /// 创建 session（记录启动位置 cwd），返回 session id（UUID v7 字符串）。
    pub async fn create_session(&self, cwd: impl AsRef<Path>) -> Result<String, SessionError> {
        let id = uuid::Uuid::now_v7().to_string();
        sqlx::query("INSERT INTO sessions (id, cwd) VALUES (?, ?)")
            .bind(&id)
            .bind(cwd.as_ref().to_string_lossy().as_ref())
            .execute(&self.pool)
            .await?;
        Ok(id)
    }

    /// 追加一条消息，返回新 entry id。
    ///
    /// - `parent_id` 为 `None` 时自动链到该 session 当前最新 entry
    ///   （顺序会话无需关心树结构）；为 `Some` 时必须指向本 session 的既有 entry
    /// - 同事务内更新 `sessions.first_message_at`（仅首条）与 `last_message_at`
    pub async fn append_message(
        &self,
        session_id: &str,
        parent_id: Option<&str>,
        message: &Message,
    ) -> Result<String, SessionError> {
        let mut tx = self.pool.begin().await?;

        if !session_exists(&mut tx, session_id).await? {
            return Err(SessionError::SessionNotFound(session_id.to_string()));
        }

        let parent: Option<String> = match parent_id {
            Some(parent) => {
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM entries WHERE id = ? AND session_id = ?)",
                )
                .bind(parent)
                .bind(session_id)
                .fetch_one(&mut *tx)
                .await?;
                if !exists {
                    return Err(SessionError::EntryNotFound(parent.to_string()));
                }
                Some(parent.to_string())
            }
            None => sqlx::query_scalar::<_, Option<String>>(
                "SELECT id FROM entries WHERE session_id = ? ORDER BY rowid DESC LIMIT 1",
            )
            .bind(session_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten(),
        };

        let id = uuid::Uuid::now_v7().to_string();
        let payload = serde_json::to_string(message)?;
        let timestamp = to_i64(message_timestamp(message));
        sqlx::query(
            "INSERT INTO entries (id, session_id, parent_id, role, timestamp, payload)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(session_id)
        .bind(parent)
        .bind(message_role(message))
        .bind(timestamp)
        .bind(payload)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE sessions SET
                 first_message_at = COALESCE(first_message_at, ?),
                 last_message_at = ?
             WHERE id = ?",
        )
        .bind(timestamp)
        .bind(timestamp)
        .bind(session_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(id)
    }

    /// 加载默认分支的完整消息序列。
    ///
    /// 取该 session 全部 entries 在内存建树，从根沿"每级最新子节点"走到叶子
    /// （默认分支语义；未来 branch 切换走显式 entry id，无需迁移）。
    pub async fn load_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionError> {
        let mut tx = self.pool.begin().await?;
        if !session_exists(&mut tx, session_id).await? {
            return Err(SessionError::SessionNotFound(session_id.to_string()));
        }
        let rows = sqlx::query(
            "SELECT id, parent_id, payload FROM entries WHERE session_id = ? ORDER BY rowid",
        )
        .bind(session_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        // parent_id -> 按插入序排列的子节点
        let mut children: HashMap<Option<String>, Vec<(String, String)>> = HashMap::new();
        for row in &rows {
            let id: String = row.get("id");
            let parent_id: Option<String> = row.get("parent_id");
            let payload: String = row.get("payload");
            children.entry(parent_id).or_default().push((id, payload));
        }

        let mut messages = Vec::new();
        let mut cursor: Option<String> = None; // 从根（parent_id IS NULL）出发
        while let Some(siblings) = children.get(&cursor) {
            // 同级取最新子节点（rowid 升序排列的最后一个）
            let Some((id, payload)) = siblings.last() else {
                break;
            };
            messages.push(serde_json::from_str::<Message>(payload)?);
            cursor = Some(id.clone());
        }
        Ok(messages)
    }

    /// 列出全部 session 摘要（按末条消息时间降序，无消息的排最后）。
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let rows = sqlx::query(
            "SELECT s.id, s.cwd, s.first_message_at, s.last_message_at,
                    (SELECT COUNT(*) FROM entries e WHERE e.session_id = s.id) AS message_count
             FROM sessions s
             ORDER BY s.last_message_at IS NULL, s.last_message_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut summaries = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row.get("id");
            let cwd: String = row.get("cwd");
            let first: Option<i64> = row.get("first_message_at");
            let last: Option<i64> = row.get("last_message_at");
            let count: i64 = row.get("message_count");
            summaries.push(SessionSummary {
                id,
                cwd: PathBuf::from(cwd),
                first_message_at: first.map(to_u64),
                last_message_at: last.map(to_u64),
                message_count: to_u64(count),
            });
        }
        Ok(summaries)
    }
}

/// 默认库路径：`$XDG_DATA_HOME/nomic/sessions.db`，fallback `~/.local/share/nomic/sessions.db`。
///
/// 手写解析 XDG，不引入 `dirs` 依赖；无 `HOME` 时返回 [`SessionError::Io`]。
pub fn default_db_path() -> Result<PathBuf, SessionError> {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("nomic").join("sessions.db"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve default db path: neither XDG_DATA_HOME nor HOME is set",
        )
    })?;
    Ok(PathBuf::from(home)
        .join(".local/share")
        .join("nomic")
        .join("sessions.db"))
}

async fn session_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?)")
        .bind(session_id)
        .fetch_one(&mut **tx)
        .await
}

const fn message_role(message: &Message) -> &'static str {
    match message {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::ToolResult(_) => "tool_result",
    }
}

const fn message_timestamp(message: &Message) -> u64 {
    match message {
        Message::User(m) => m.timestamp,
        Message::Assistant(m) => m.timestamp,
        Message::ToolResult(m) => m.timestamp,
    }
}

/// Unix 毫秒时间戳在可预见的未来不会超出 i64 范围
#[allow(clippy::cast_possible_wrap)]
const fn to_i64(timestamp: u64) -> i64 {
    timestamp as i64
}

/// 库中时间戳/计数均由本 crate 写入，保证非负
#[allow(clippy::cast_sign_loss)]
const fn to_u64(value: i64) -> u64 {
    value as u64
}
