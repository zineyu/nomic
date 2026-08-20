//! nomic-session：SQLite session 持久化（M2，替代 pi 的 JSONL session 文件）。
//!
//! - 每个 session 一个唯一 id（UUID v7，时间有序）；session 创建时绑定
//!   workspace（文件系统路径的一等实体，见 [`Workspace`]），其所有操作以
//!   workspace 路径为基准
//! - 消息存 `entries` 表，按 `parent_id` 组织为**树**（顺序会话是树的特例）；
//!   分支能力：[`SessionStore::list_tree`] 浏览树、[`SessionStore::load_branch`]
//!   加载指定 entry 所在分支、追加时显式 `parent_id` 即创建分支
//! - 压缩条目（`kind = 'compaction'`）记录上下文压缩结果；加载时重放重建
//!   有效上下文（重建语义见 `nomic_ai::compaction` module 文档）
//! - `config` 表存配置历史（append-only，实现见 `config` 模块）：每次修改
//!   新增一行（含更新时间戳），读取方从最新一行向最老一行逐步回退
//!   （feedback），直到无可回退的行为止；值用 sqlite 原生 JSON 类型（JSONB）存储
//! - 全局单库，默认位于平台标准 data 目录下的 `nomic/sessions.db`
//!   （由 `dirs` 解析，见 [`default_db_path`]）
//! - [`SessionRecorder`] 把落库策略（定稿点、落什么、父指针推进）收在
//!   事件流 seam 后面：print / TUI 只做一行接线，语义不再漂移
//!
//! 消息 payload 原样存 [`Message`] 的 serde JSON；`role`/`timestamp` 为提取列，
//! 供查询与维护 session 时间字段。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nomic_ai::{
    AssistantContent, Message, StopReason, UserContent, UserMessageContent, apply_compaction,
    now_millis,
};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row as _, SqlitePool};

mod config;
mod recorder;
mod workspace;
pub use recorder::SessionRecorder;
pub use workspace::{Workspace, WorkspaceSummary};

/// 内嵌迁移（`crates/runtime/nomic-session/migrations/`）。
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// 压缩条目（`entries.kind = 'compaction'`）的 payload。
///
/// 记录一次上下文压缩的结果：摘要正文、保留的近期消息条数与压缩前的
/// token 估算。重建语义（`kept_count` 相对计数代替 pi 的
/// `first_kept_entry_id` 绝对指针、重复压缩的递归成立性、分支路径重放的
/// 精确性）唯一定义于 `nomic_ai::compaction` module，加载路径经
/// [`nomic_ai::apply_compaction`] 应用（见 `docs/adr/0005`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionRecord {
    /// 结构化摘要（含 `<read-files>` / `<modified-files>` 附加段）
    pub summary: String,
    /// 压缩时保留的近期消息条数（相对压缩前的有效上下文计数）
    pub kept_count: u64,
    /// 压缩前的上下文 token 估算
    pub tokens_before: u64,
}

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
    /// workspace 不存在（`get_or_create_workspace` 登记后读取仍缺失，
    /// 仅在库被并发破坏时可能出现）
    #[error("workspace not found: {0}")]
    WorkspaceNotFound(String),
    /// entry id 不存在（或不属于目标 session）
    #[error("entry not found: {0}")]
    EntryNotFound(String),
    /// 库中 payload 不是合法的 [`Message`] JSON（数据损坏）
    #[error("message payload corrupted: {0}")]
    Corrupt(#[from] serde_json::Error),
}

/// session 摘要（`list_sessions` / `list_sessions_in` 返回）。
///
/// `id` 为内部标识（UUID v7），不对用户展示；用户可见的名称是
/// [`Self::title`]（首条 user 消息的首行摘要）。
/// 派生 serde（web 模式经 REST 列表给前端会话侧栏）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionSummary {
    /// session id（UUID v7；内部标识，不展示给用户）
    pub id: String,
    /// 会话标题：首条 user 消息的首行摘要（无消息时为 `None`，展示侧自行回退）
    pub title: Option<String>,
    /// 所属 workspace id
    pub workspace_id: String,
    /// 所属 workspace 路径（session 操作的基准目录）
    pub workspace: PathBuf,
    /// 首条消息时间（Unix 毫秒；无消息时为 `None`）
    pub first_message_at: Option<u64>,
    /// 末条消息时间（Unix 毫秒；无消息时为 `None`）
    pub last_message_at: Option<u64>,
    /// 消息总数
    pub message_count: u64,
}

/// 从消息序列计算会话标题：第一条含正文的 user 消息的首行摘要
/// （[`first_line`] 截断）；无符合条件的消息时为 `None`。
///
/// 会话的用户可见名称；session id（UUID）只作内部标识，不展示。
pub fn session_title(messages: &[Message]) -> Option<String> {
    messages.iter().find_map(|message| match message {
        Message::User(_) => {
            let (preview, _) = message_preview(message);
            (!preview.is_empty()).then_some(preview)
        }
        _ => None,
    })
}

/// 首行截断到 40 字符（会话标题、树列表等单行展示用）。
pub fn first_line(text: &str) -> String {
    const MAX_CHARS: usize = 40;
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= MAX_CHARS {
        line.to_string()
    } else {
        let truncated: String = line.chars().take(MAX_CHARS).collect();
        format!("{truncated}…")
    }
}

/// 会话树条目（[`SessionStore::list_tree`] 返回，分支浏览与选择用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// entry id（UUID v7）
    pub id: String,
    /// 父 entry id（根为 `None`）
    pub parent_id: Option<String>,
    /// 条目角色：user / assistant / tool_result / compaction
    pub role: String,
    /// 落库时间（Unix 毫秒）
    pub timestamp: u64,
    /// 单行内容摘要（展示用；payload 损坏时为占位文本）
    pub preview: String,
    /// assistant 条目是否含工具调用块（其余角色恒为 `false`）。
    /// 含工具调用的条目不可作为分支起点：从其分叉会把悬空的 tool_use
    /// 留在上下文里（对应 tool_result 不在新分支路径上），provider 会拒绝。
    pub has_tool_calls: bool,
}

impl TreeEntry {
    /// 是否可作为分支起点（非工具调用条目：工具结果与含工具调用的
    /// assistant 响应除外，理由见 [`Self::has_tool_calls`]）。
    pub fn is_branchable(&self) -> bool {
        self.role != "tool_result" && !self.has_tool_calls
    }
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

    /// 打开默认路径（平台标准 data 目录）的库，见 [`default_db_path`]。
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

    /// 创建 session：登记（或复用）路径对应的 workspace 并绑定，返回
    /// session id（UUID v7 字符串）。显式持有 workspace id 的调用方用
    /// [`Self::create_session_in`]。
    pub async fn create_session(
        &self,
        workspace: impl AsRef<Path>,
    ) -> Result<String, SessionError> {
        let workspace = self.get_or_create_workspace(workspace).await?;
        self.create_session_in(&workspace.id).await
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
        let payload = serde_json::to_string(message)?;
        self.append_entry(
            session_id,
            parent_id,
            "message",
            message_role(message),
            message_timestamp(message),
            payload,
        )
        .await
    }

    /// 追加一条压缩条目，返回新 entry id。
    ///
    /// 链接语义与 [`Self::append_message`] 一致；同事务内更新
    /// `sessions.last_message_at`。
    pub async fn append_compaction(
        &self,
        session_id: &str,
        parent_id: Option<&str>,
        record: &CompactionRecord,
    ) -> Result<String, SessionError> {
        let payload = serde_json::to_string(record)?;
        self.append_entry(
            session_id,
            parent_id,
            "compaction",
            "compaction",
            now_millis(),
            payload,
        )
        .await
    }

    /// 追加一条 entry 的实现内核（消息与压缩条目共用）：同事务内校验
    /// session 存在、解析父指针、插入条目并维护 `sessions` 的首/末消息时间。
    async fn append_entry(
        &self,
        session_id: &str,
        parent_id: Option<&str>,
        kind: &str,
        role: &str,
        timestamp: u64,
        payload: String,
    ) -> Result<String, SessionError> {
        // `BEGIN IMMEDIATE` 在事务一开始就取得写锁，避免 WAL 模式下「先读后写」
        // 升级写锁时因另一连接已提交而产生的 SQLITE_BUSY_SNAPSHOT（code 517）：
        // 该错误不会被 busy_timeout 重试，只能靠预先取写锁规避。多实例并发写入
        // 时此竞争必然触发，见 `tests/concurrency.rs::concurrent_writers_across_pools`。
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        if !session_exists(&mut tx, session_id).await? {
            return Err(SessionError::SessionNotFound(session_id.to_string()));
        }

        let parent = resolve_parent(&mut tx, session_id, parent_id).await?;

        let id = uuid::Uuid::now_v7().to_string();
        let timestamp = to_i64(timestamp);
        sqlx::query(
            "INSERT INTO entries (id, session_id, parent_id, role, timestamp, payload, kind)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(session_id)
        .bind(parent)
        .bind(role)
        .bind(timestamp)
        .bind(payload)
        .bind(kind)
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

        Self::touch_workspace(&mut tx, session_id, to_u64(timestamp)).await?;

        tx.commit().await?;
        Ok(id)
    }

    /// 加载默认分支的完整消息序列。
    ///
    /// 取该 session 全部 entries 在内存建树，从根沿"每级最新子节点"走到叶子
    /// （默认分支语义；branch 切换走显式 entry id，见 [`Self::load_branch`]）。
    pub async fn load_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionError> {
        let entries = self.fetch_entries(session_id).await?;
        replay(default_path(&entries))
    }

    /// 加载指定 entry 所在分支的完整消息序列：沿 `entry_id` 的祖先链
    /// （根 → 该 entry）重放。branch 切换（`/tree` 选择分支起点）的加载路径。
    ///
    /// 重放沿祖先路径线性进行，路径前缀即压缩发生时 agent 实际持有的上下文，
    /// 因此 `kept_count` 相对计数在任意分支路径上依然精确（见
    /// `nomic_ai::compaction` module 文档）。
    pub async fn load_branch(
        &self,
        session_id: &str,
        entry_id: &str,
    ) -> Result<Vec<Message>, SessionError> {
        let entries = self.fetch_entries(session_id).await?;
        let path = ancestor_path(&entries, entry_id)
            .ok_or_else(|| SessionError::EntryNotFound(entry_id.to_string()))?;
        replay(path)
    }

    /// 默认分支的末端 entry id（无条目时为 `None`）。
    ///
    /// 恢复 session 后初始化落库父指针用：session 尚无分支时与「链到最新
    /// entry」等价，存在分支后显式父指针保证续写落在默认分支而非全局最新
    /// entry。
    pub async fn latest_entry_id(&self, session_id: &str) -> Result<Option<String>, SessionError> {
        let entries = self.fetch_entries(session_id).await?;
        Ok(default_path(&entries).last().map(|entry| entry.id.clone()))
    }

    /// 列出 session 的全部条目（按插入序），供会话树浏览与分支起点选择。
    ///
    /// 条目含单行内容摘要（展示用）；payload 损坏不报错，摘要退化为占位
    /// 文本（浏览是只读操作，不应被单条损坏数据整体阻断）。
    pub async fn list_tree(&self, session_id: &str) -> Result<Vec<TreeEntry>, SessionError> {
        let entries = self.fetch_entries(session_id).await?;
        Ok(entries.iter().map(tree_entry).collect())
    }

    /// 读取 session 的全部 entries（按插入序）；session 不存在时报
    /// [`SessionError::SessionNotFound`]。
    async fn fetch_entries(&self, session_id: &str) -> Result<Vec<BranchEntry>, SessionError> {
        let mut tx = self.pool.begin().await?;
        if !session_exists(&mut tx, session_id).await? {
            return Err(SessionError::SessionNotFound(session_id.to_string()));
        }
        let rows = sqlx::query(
            "SELECT id, parent_id, role, kind, timestamp, payload FROM entries
             WHERE session_id = ? ORDER BY rowid",
        )
        .bind(session_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in &rows {
            entries.push(BranchEntry {
                id: row.get("id"),
                parent_id: row.get("parent_id"),
                role: row.get("role"),
                kind: row.get("kind"),
                timestamp: to_u64(row.get("timestamp")),
                payload: row.get("payload"),
            });
        }
        Ok(entries)
    }

    /// 列出全部 session 摘要（按末条消息时间降序，无消息的排最后）。
    pub async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        self.summarize(None).await
    }

    /// 列出指定 workspace 下的 session 摘要（排序同 [`Self::list_sessions`]）。
    pub async fn list_sessions_in(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        self.summarize(Some(workspace_id)).await
    }

    /// session 摘要查询内核：可选按 workspace 过滤；标题经分组查询批量补齐。
    async fn summarize(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<SessionSummary>, SessionError> {
        let rows = sqlx::query(
            "SELECT s.id, s.workspace_id, w.path AS workspace_path,
                    s.first_message_at, s.last_message_at,
                    (SELECT COUNT(*) FROM entries e
                     WHERE e.session_id = s.id AND e.kind = 'message') AS message_count
             FROM sessions s JOIN workspaces w ON w.id = s.workspace_id
             WHERE (?1 IS NULL OR s.workspace_id = ?1)
             ORDER BY s.last_message_at IS NULL, s.last_message_at DESC",
        )
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await?;

        let titles = self.fetch_titles().await?;
        let mut summaries = Vec::with_capacity(rows.len());
        for row in &rows {
            let id: String = row.get("id");
            let workspace_path: String = row.get("workspace_path");
            let first: Option<i64> = row.get("first_message_at");
            let last: Option<i64> = row.get("last_message_at");
            let count: i64 = row.get("message_count");
            summaries.push(SessionSummary {
                title: titles.get(&id).cloned(),
                id,
                workspace_id: row.get("workspace_id"),
                workspace: PathBuf::from(workspace_path),
                first_message_at: first.map(to_u64),
                last_message_at: last.map(to_u64),
                message_count: to_u64(count),
            });
        }
        Ok(summaries)
    }

    /// 各 session 的标题（首条 user 消息摘要）：一次分组查询取每个 session
    /// 最早一条 user 消息的 payload，在内存计算摘要；payload 损坏或无 user
    /// 消息的 session 不出现在结果中（标题为 `None`）。
    async fn fetch_titles(&self) -> Result<HashMap<String, String>, SessionError> {
        let rows = sqlx::query(
            "SELECT e.session_id, e.payload FROM entries e
             JOIN (SELECT session_id, MIN(rowid) AS first_rowid FROM entries
                   WHERE kind = 'message' AND role = 'user' GROUP BY session_id) f
             ON e.rowid = f.first_rowid",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut titles = HashMap::with_capacity(rows.len());
        for row in &rows {
            let payload: String = row.get("payload");
            let title = serde_json::from_str::<Message>(&payload)
                .ok()
                .and_then(|message| session_title(std::slice::from_ref(&message)));
            if let Some(title) = title {
                titles.insert(row.get::<String, _>("session_id"), title);
            }
        }
        Ok(titles)
    }
}

/// 默认库路径：平台标准 data 目录下的 `nomic/sessions.db`（由 `dirs` 解析：
/// Linux 为 `$XDG_DATA_HOME` 或 `~/.local/share`，macOS 为 `~/Library/Application Support`）。
///
/// 无法解析标准目录时返回 [`SessionError::Io`]。
pub fn default_db_path() -> Result<PathBuf, SessionError> {
    let dir = dirs::data_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve default db path: no platform data directory",
        )
    })?;
    Ok(dir.join("nomic").join("sessions.db"))
}

/// 分支重放用的一行 entry（`fetch_entries` 内部表示）。
struct BranchEntry {
    id: String,
    parent_id: Option<String>,
    role: String,
    kind: String,
    timestamp: u64,
    payload: String,
}

/// 默认分支路径：从根沿「每级最新子节点」（rowid 升序排列的最后一个）走到叶子。
fn default_path(entries: &[BranchEntry]) -> Vec<&BranchEntry> {
    // parent_id -> 按插入序排列的子节点
    let mut children: HashMap<Option<&str>, Vec<&BranchEntry>> = HashMap::new();
    for entry in entries {
        children
            .entry(entry.parent_id.as_deref())
            .or_default()
            .push(entry);
    }
    let mut path = Vec::new();
    let mut cursor: Option<&str> = None; // 从根（parent_id IS NULL）出发
    while let Some(siblings) = children.get(&cursor) {
        let Some(&entry) = siblings.last() else {
            break;
        };
        path.push(entry);
        cursor = Some(entry.id.as_str());
    }
    path
}

/// `entry_id` 的祖先路径（根 → 该 entry）；entry 不属于这批 entries 时为 `None`。
///
/// 父指针在写入时校验存在且 entries 不可变，父链 rowid 严格递减，回溯必然终止。
fn ancestor_path<'a>(entries: &'a [BranchEntry], entry_id: &str) -> Option<Vec<&'a BranchEntry>> {
    let by_id: HashMap<&str, &BranchEntry> = entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    let mut path = Vec::new();
    let mut cursor = Some(entry_id);
    while let Some(id) = cursor {
        let entry = by_id.get(id)?;
        path.push(*entry);
        cursor = entry.parent_id.as_deref();
    }
    path.reverse();
    Some(path)
}

/// 沿路径重放重建有效上下文：message 直接追加；compaction 条目经
/// `apply_compaction` 应用（语义见 `nomic_ai::compaction`）。
fn replay(path: Vec<&BranchEntry>) -> Result<Vec<Message>, SessionError> {
    let mut effective: Vec<Message> = Vec::new();
    for entry in path {
        if entry.kind == "compaction" {
            let record: CompactionRecord = serde_json::from_str(&entry.payload)?;
            effective = apply_compaction(
                &effective,
                &record.summary,
                record.kept_count,
                entry.timestamp,
            );
        } else {
            effective.push(serde_json::from_str::<Message>(&entry.payload)?);
        }
    }
    Ok(effective)
}

/// `BranchEntry` → 展示用的 [`TreeEntry`]（payload 损坏时摘要退化为占位文本）。
fn tree_entry(entry: &BranchEntry) -> TreeEntry {
    let (preview, has_tool_calls) = if entry.kind == "compaction" {
        let preview = serde_json::from_str::<CompactionRecord>(&entry.payload).map_or_else(
            |_| "（payload 损坏）".to_string(),
            |record| format!("上下文压缩（保留 {} 条近期消息）", record.kept_count),
        );
        (preview, false)
    } else {
        serde_json::from_str::<Message>(&entry.payload).map_or_else(
            |_| ("（payload 损坏）".to_string(), false),
            |m| message_preview(&m),
        )
    };
    TreeEntry {
        id: entry.id.clone(),
        parent_id: entry.parent_id.clone(),
        role: entry.role.clone(),
        timestamp: entry.timestamp,
        preview,
        has_tool_calls,
    }
}

/// 消息的单行内容摘要与工具调用标记（`list_tree` 展示用）。
fn message_preview(message: &Message) -> (String, bool) {
    match message {
        Message::User(user) => {
            let (text, images) = match &user.content {
                UserMessageContent::Text(text) => (text.clone(), 0),
                UserMessageContent::Blocks(blocks) => {
                    let text = blocks
                        .iter()
                        .filter_map(|block| match block {
                            UserContent::Text(text) => Some(text.text.as_str()),
                            UserContent::Image(_) => None,
                        })
                        .collect::<String>();
                    let images = blocks
                        .iter()
                        .filter(|block| matches!(block, UserContent::Image(_)))
                        .count();
                    (text, images)
                }
            };
            let preview = if images == 0 {
                first_line(&text)
            } else {
                format!("🖼 图片 ×{images} {}", first_line(&text))
            };
            (preview.trim().to_string(), false)
        }
        Message::Assistant(assistant) => {
            let has_tool_calls = assistant
                .content
                .iter()
                .any(|content| matches!(content, AssistantContent::ToolCall(_)));
            if matches!(
                assistant.stop_reason,
                StopReason::Error | StopReason::Aborted
            ) {
                let detail = assistant.error_message.as_deref().unwrap_or("未知错误");
                return (
                    format!("（响应失败：{}）", first_line(detail)),
                    has_tool_calls,
                );
            }
            let text = assistant.content.iter().find_map(|content| match content {
                AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                    Some(first_line(&text.text))
                }
                _ => None,
            });
            let preview = match text {
                Some(text) => text,
                None if has_tool_calls => "（工具调用）".to_string(),
                None => "（空响应）".to_string(),
            };
            (preview, has_tool_calls)
        }
        Message::ToolResult(result) => {
            let marker = if result.is_error { "失败" } else { "结果" };
            (format!("工具{marker}：{}", result.tool_name), false)
        }
    }
}

/// 解析追加操作的父 entry：`Some` 时校验其属于本 session（不存在报
/// [`SessionError::EntryNotFound`]）；`None` 时链到该 session 当前最新 entry。
async fn resolve_parent(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: &str,
    parent_id: Option<&str>,
) -> Result<Option<String>, SessionError> {
    match parent_id {
        Some(parent) => {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM entries WHERE id = ? AND session_id = ?)",
            )
            .bind(parent)
            .bind(session_id)
            .fetch_one(&mut **tx)
            .await?;
            if !exists {
                return Err(SessionError::EntryNotFound(parent.to_string()));
            }
            Ok(Some(parent.to_string()))
        }
        None => Ok(sqlx::query_scalar::<_, Option<String>>(
            "SELECT id FROM entries WHERE session_id = ? ORDER BY rowid DESC LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(&mut **tx)
        .await?
        .flatten()),
    }
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
pub(crate) const fn to_i64(timestamp: u64) -> i64 {
    timestamp as i64
}

/// 库中时间戳/计数均由本 crate 写入，保证非负
#[allow(clippy::cast_sign_loss)]
pub(crate) const fn to_u64(value: i64) -> u64 {
    value as u64
}
