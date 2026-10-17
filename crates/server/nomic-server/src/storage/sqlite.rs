//! SQLite 存储实现：[`nomic_core::Storage`] trait 的具体实现。
//!
//! 领域对象整体经 serde 序列化落库；id、状态、步骤类别等查询维度
//! 以独立列投影，SQL 不解析 JSON 体。所有操作经 `spawn_blocking`
//! 在阻塞线程池执行：rusqlite 是同步接口，单连接 + 互斥锁串行化写
//! （SQLite 单写者语义），调用方获得异步接口。打开时完成 schema
//! 版本迁移（见 `schema` 子模块）。

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use nomic_core::{
    Acceptance, Message, MessageId, ParentRef, Session, SessionId, SessionState,
    SessionTreeSnapshot, Step, Storage, StorageError, Submission, ToolCallId,
};
use rusqlite::{Connection, OptionalExtension, Row};
use tokio::task::spawn_blocking;

use super::schema;

/// 基于 SQLite 的 [`Storage`] 实现。
#[derive(Debug)]
pub struct SQLiteStorage {
    conn: Arc<Mutex<Connection>>,
}

impl SQLiteStorage {
    /// 打开（必要时创建）`path` 处的 SQLite 库并迁移到当前 schema。
    ///
    /// # Errors
    ///
    /// 打不开文件或迁移失败时返回对应的 [`StorageError`]；库的版本
    /// 高于当前实现时返回 [`StorageError::Migration`]。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let mut conn =
            Connection::open(path).map_err(|error| StorageError::Connection(error.to_string()))?;
        schema::migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
}

/// 在阻塞线程池里执行一次存储操作：串行化由连接上的互斥锁保证。
fn blocking<T, F>(
    conn: &Arc<Mutex<Connection>>,
    f: F,
) -> impl Future<Output = Result<T, StorageError>> + Send
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, StorageError> + Send + 'static,
{
    let conn = Arc::clone(conn);
    async move {
        spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|error| StorageError::Connection(error.to_string()))?;
            f(&mut guard)
        })
        .await
        .map_err(|error| StorageError::Connection(error.to_string()))?
    }
}

/// sessions 表的一行（字符串形态）；SQL 与领域解析之间的边界。
struct SessionRow {
    id: String,
    parent_session: Option<String>,
    forked_at: Option<String>,
    working_dir: String,
    state: String,
}

const SESSION_COLUMNS: &str = "id, parent_session, forked_at, working_dir, state";

/// 从结果行读取 sessions 列（字符串形态）。
fn session_row(row: &Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        parent_session: row.get(1)?,
        forked_at: row.get(2)?,
        working_dir: row.get(3)?,
        state: row.get(4)?,
    })
}

fn parse_id<T: FromStr>(text: &str) -> Result<T, StorageError>
where
    T::Err: fmt::Display,
{
    text.parse::<T>()
        .map_err(|error| StorageError::Query(format!("corrupt id {text}: {error}")))
}

/// 会话状态的落库编码。
const fn state_code(state: SessionState) -> &'static str {
    match state {
        SessionState::Pending => "pending",
        SessionState::Running => "running",
        SessionState::Idle => "idle",
    }
}

/// 落库编码还原为会话状态。
fn parse_state(code: &str) -> Result<SessionState, StorageError> {
    match code {
        "pending" => Ok(SessionState::Pending),
        "running" => Ok(SessionState::Running),
        "idle" => Ok(SessionState::Idle),
        other => Err(StorageError::Query(format!(
            "corrupt session row: unknown state {other}"
        ))),
    }
}

fn parse_session(row: SessionRow) -> Result<Session, StorageError> {
    let parent = match (row.parent_session, row.forked_at) {
        (Some(session), Some(forked_at)) => Some(ParentRef {
            session: parse_id(&session)?,
            forked_at: parse_id(&forked_at)?,
        }),
        (None, None) => None,
        _ => {
            return Err(StorageError::Query(
                "corrupt session row: parent reference incomplete".into(),
            ));
        }
    };
    Ok(Session::restore(
        parse_id(&row.id)?,
        parent,
        Path::new(&row.working_dir).to_path_buf(),
        parse_state(&row.state)?,
    ))
}

/// 插入一条会话记录；工作目录必须是合法 UTF-8（存储边界显式失败）。
fn insert_session(conn: &Connection, session: &Session) -> Result<(), StorageError> {
    let dir = session
        .working_dir()
        .to_str()
        .ok_or_else(|| StorageError::Query("working dir is not valid UTF-8".into()))?;
    let (parent, forked_at) = match session.parent() {
        Some(ParentRef {
            session: parent,
            forked_at,
        }) => (Some(parent.to_string()), Some(forked_at.to_string())),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO sessions (id, parent_session, forked_at, working_dir, state) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        (
            session.id().to_string(),
            parent,
            forked_at,
            dir,
            state_code(session.state()),
        ),
    )
    .map_err(|error| StorageError::Query(error.to_string()))?;
    Ok(())
}

fn load_session(conn: &Connection, id: SessionId) -> Result<Option<Session>, StorageError> {
    let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1");
    let row = conn
        .query_row(&sql, [id.to_string()], session_row)
        .optional()
        .map_err(|error| StorageError::Query(error.to_string()))?;
    row.map(parse_session).transpose()
}

fn session_exists(conn: &Connection, id: SessionId) -> Result<(), StorageError> {
    let found: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
            [id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| StorageError::Query(error.to_string()))?;
    if found {
        Ok(())
    } else {
        Err(StorageError::NotFound(format!("session {id}")))
    }
}

/// 会话在 `table`（steps 或 messages）中的下一个序号。
fn next_ordinal(
    conn: &Connection,
    table: &'static str,
    session: SessionId,
) -> Result<i64, StorageError> {
    let sql = format!("SELECT COALESCE(MAX(ordinal) + 1, 0) FROM {table} WHERE session_id = ?1");
    conn.query_row(&sql, [session.to_string()], |row| row.get(0))
        .map_err(|error| StorageError::Query(error.to_string()))
}

/// 步骤类别的落库编码：steps.kind 列的唯一事实源。
///（迁移 fixture 固化的 v1 契约除外，它不随实现演进。）
/// 会话子树的递归查询（删除级联用）。
const SUBTREE: &str = "WITH RECURSIVE subtree(id) AS ( \
     SELECT id FROM sessions WHERE id = ?1 \
     UNION ALL \
     SELECT s.id FROM sessions s JOIN subtree t ON s.parent_session = t.id) \
     SELECT id FROM subtree";

const KIND_MESSAGE: &str = "message";
const KIND_TOOL_CALL_STARTED: &str = "tool_call_started";
const KIND_TOOL_CALL_FINISHED: &str = "tool_call_finished";
const KIND_TOOL_CALL_INTERRUPTED: &str = "tool_call_interrupted";

/// 步骤的落库投影：类别、关联调用与关联消息（body 之外的可查询维度）。
struct StepMeta {
    kind: &'static str,
    call_id: Option<ToolCallId>,
    message_id: Option<MessageId>,
}

const fn step_meta(step: &Step) -> StepMeta {
    match step {
        Step::Message(message) => StepMeta {
            kind: KIND_MESSAGE,
            call_id: None,
            message_id: Some(message.id()),
        },
        Step::ToolCallStarted { call } => StepMeta {
            kind: KIND_TOOL_CALL_STARTED,
            call_id: Some(call.id),
            message_id: None,
        },
        Step::ToolCallFinished { call_id, .. } => StepMeta {
            kind: KIND_TOOL_CALL_FINISHED,
            call_id: Some(*call_id),
            message_id: None,
        },
        Step::ToolCallInterrupted { call_id, .. } => StepMeta {
            kind: KIND_TOOL_CALL_INTERRUPTED,
            call_id: Some(*call_id),
            message_id: None,
        },
    }
}

/// 向会话追加一个步骤（步骤日志）。
fn insert_step(conn: &Connection, session: SessionId, step: &Step) -> Result<(), StorageError> {
    let meta = step_meta(step);
    let body =
        serde_json::to_string(step).map_err(|error| StorageError::Query(error.to_string()))?;
    let ordinal = next_ordinal(conn, "steps", session)?;
    conn.execute(
        "INSERT INTO steps (session_id, ordinal, kind, call_id, message_id, body) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        (
            session.to_string(),
            ordinal,
            meta.kind,
            meta.call_id.map(|id| id.to_string()),
            meta.message_id.map(|id| id.to_string()),
            body,
        ),
    )
    .map_err(|error| StorageError::Query(error.to_string()))?;
    Ok(())
}

/// 向会话追加一条消息（消息表与步骤日志同写）。
fn insert_message(
    conn: &Connection,
    session: SessionId,
    message: &Message,
) -> Result<(), StorageError> {
    let body =
        serde_json::to_string(message).map_err(|error| StorageError::Query(error.to_string()))?;
    let ordinal = next_ordinal(conn, "messages", session)?;
    conn.execute(
        "INSERT INTO messages (session_id, ordinal, message_id, body) VALUES (?1, ?2, ?3, ?4)",
        (session.to_string(), ordinal, message.id().to_string(), body),
    )
    .map_err(|error| StorageError::Query(error.to_string()))?;
    Ok(())
}

impl Storage for SQLiteStorage {
    fn record_submission(
        &self,
        submission: &Submission,
    ) -> impl Future<Output = Result<Acceptance, StorageError>> + Send {
        let submission = submission.clone();
        blocking(&self.conn, move |conn| {
            let tx = conn
                .transaction()
                .map_err(|error| StorageError::Query(error.to_string()))?;

            // 台账按 request_id 去重：重复提交返回原受理结果
            let ledger: Option<String> = tx
                .query_row(
                    "SELECT session_id FROM submissions WHERE request_id = ?1",
                    [submission.request_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| StorageError::Query(error.to_string()))?;
            if let Some(session) = ledger {
                tx.commit()
                    .map_err(|error| StorageError::Query(error.to_string()))?;
                return Ok(Acceptance::Duplicate {
                    session_id: parse_id(&session)?,
                });
            }

            let (session_id, created) = match &submission.target {
                nomic_core::SessionTarget::New => {
                    let session = Session::restore(
                        SessionId::new(),
                        None,
                        submission.working_dir.clone(),
                        SessionState::Pending,
                    );
                    insert_session(&tx, &session)?;
                    (session.id(), true)
                }
                nomic_core::SessionTarget::Existing(id) => {
                    session_exists(&tx, *id)?;
                    (*id, false)
                }
            };

            // 用户消息排入会话（步骤日志与消息表同写）
            let message = Message::User {
                id: MessageId::new(),
                text: submission.content,
            };
            insert_step(&tx, session_id, &Step::Message(message.clone()))?;
            insert_message(&tx, session_id, &message)?;
            // 既有会话受理后回到待处理；新建会话落库时已是待处理
            if !created {
                tx.execute(
                    "UPDATE sessions SET state = ?1 WHERE id = ?2",
                    (state_code(SessionState::Pending), session_id.to_string()),
                )
                .map_err(|error| StorageError::Query(error.to_string()))?;
            }
            tx.execute(
                "INSERT INTO submissions (request_id, session_id) VALUES (?1, ?2)",
                (submission.request_id.to_string(), session_id.to_string()),
            )
            .map_err(|error| StorageError::Query(error.to_string()))?;
            tx.commit()
                .map_err(|error| StorageError::Query(error.to_string()))?;
            Ok(Acceptance::Accepted { session_id })
        })
    }

    fn create_session(
        &self,
        working_dir: std::path::PathBuf,
    ) -> impl Future<Output = Result<Session, StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            let session = Session::restore(SessionId::new(), None, working_dir, SessionState::Idle);
            insert_session(conn, &session)?;
            Ok(session)
        })
    }

    fn get_session(
        &self,
        id: SessionId,
    ) -> impl Future<Output = Result<Option<Session>, StorageError>> + Send {
        blocking(&self.conn, move |conn| load_session(conn, id))
    }

    fn append_step(
        &self,
        session: SessionId,
        step: Step,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            let tx = conn
                .transaction()
                .map_err(|error| StorageError::Query(error.to_string()))?;
            session_exists(&tx, session)?;
            insert_step(&tx, session, &step)?;
            if let Step::Message(message) = &step {
                insert_message(&tx, session, message)?;
            }
            tx.commit()
                .map_err(|error| StorageError::Query(error.to_string()))
        })
    }

    fn steps(
        &self,
        session: SessionId,
    ) -> impl Future<Output = Result<Vec<Step>, StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            session_exists(conn, session)?;
            let mut stmt = conn
                .prepare("SELECT body FROM steps WHERE session_id = ?1 ORDER BY ordinal ASC")
                .map_err(|error| StorageError::Query(error.to_string()))?;
            let bodies = stmt
                .query_map([session.to_string()], |row| row.get::<_, String>(0))
                .map_err(|error| StorageError::Query(error.to_string()))?;
            bodies
                .map(|body| {
                    serde_json::from_str(&body.map_err(|e| StorageError::Query(e.to_string()))?)
                        .map_err(|error| StorageError::Query(error.to_string()))
                })
                .collect()
        })
    }

    fn session_tree(
        &self,
    ) -> impl Future<Output = Result<SessionTreeSnapshot, StorageError>> + Send {
        blocking(&self.conn, |conn| {
            let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions ORDER BY id ASC");
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|error| StorageError::Query(error.to_string()))?;
            let rows = stmt
                .query_map([], session_row)
                .map_err(|error| StorageError::Query(error.to_string()))?;
            let mut sessions = Vec::new();
            for row in rows {
                let row = row.map_err(|error| StorageError::Query(error.to_string()))?;
                sessions.push(parse_session(row)?);
            }
            Ok(SessionTreeSnapshot::from_sessions(sessions))
        })
    }

    fn fork(
        &self,
        source: SessionId,
        at: MessageId,
    ) -> impl Future<Output = Result<Session, StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            let tx = conn
                .transaction()
                .map_err(|error| StorageError::Query(error.to_string()))?;

            // 来源会话必须存在（取其工作目录继承）
            let source_dir: String = tx
                .query_row(
                    "SELECT working_dir FROM sessions WHERE id = ?1",
                    [source.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| StorageError::Query(error.to_string()))?
                .ok_or_else(|| StorageError::NotFound(format!("session {source}")))?;
            // 分叉点必须属于来源会话
            let index: i64 = tx
                .query_row(
                    "SELECT ordinal FROM messages WHERE session_id = ?1 AND message_id = ?2",
                    (source.to_string(), at.to_string()),
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| StorageError::Query(error.to_string()))?
                .ok_or_else(|| {
                    StorageError::NotFound(format!("fork point {at} in session {source}"))
                })?;

            let child = Session::restore(
                SessionId::new(),
                Some(ParentRef {
                    session: source,
                    forked_at: at,
                }),
                Path::new(&source_dir).to_path_buf(),
                SessionState::Idle,
            );
            insert_session(&tx, &child)?;
            // 写时复制：消息前缀原序复制
            tx.execute(
                "INSERT INTO messages (session_id, ordinal, message_id, body) \
                 SELECT ?1, ordinal, message_id, body FROM messages \
                 WHERE session_id = ?2 AND ordinal <= ?3",
                (child.id().to_string(), source.to_string(), index),
            )
            .map_err(|error| StorageError::Query(error.to_string()))?;
            // 会话内容截至分叉点的步骤前缀整体同步复制（序号重排为
            // 稠密序列）：消息步骤与它们之间的工具步骤都属于共享历史，
            // 子会话只在分叉点之后重新起算执行日志。
            tx.execute(
                "INSERT INTO steps (session_id, ordinal, kind, call_id, message_id, body) \
                 SELECT ?1, ROW_NUMBER() OVER (ORDER BY ordinal) - 1, kind, call_id, message_id, body \
                 FROM steps WHERE session_id = ?2 AND ordinal <= ( \
                     SELECT MAX(ordinal) FROM steps WHERE session_id = ?2 AND message_id IN ( \
                         SELECT message_id FROM messages WHERE session_id = ?2 AND ordinal <= ?3))",
                (child.id().to_string(), source.to_string(), index),
            )
            .map_err(|error| StorageError::Query(error.to_string()))?;
            tx.commit()
                .map_err(|error| StorageError::Query(error.to_string()))?;
            Ok(child)
        })
    }

    fn mark_interrupted(
        &self,
        session: SessionId,
        call: ToolCallId,
        partial_output: Option<String>,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            let tx = conn
                .transaction()
                .map_err(|error| StorageError::Query(error.to_string()))?;
            session_exists(&tx, session)?;
            // 只接受「已派发且未完成」的调用：生命周期由 steps 的 kind 判定
            let started: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM steps WHERE session_id = ?1 AND call_id = ?2 \
                     AND kind = ?3)",
                    (
                        session.to_string(),
                        call.to_string(),
                        KIND_TOOL_CALL_STARTED,
                    ),
                    |row| row.get(0),
                )
                .map_err(|error| StorageError::Query(error.to_string()))?;
            let closed: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM steps WHERE session_id = ?1 AND call_id = ?2 \
                     AND kind IN (?3, ?4))",
                    (
                        session.to_string(),
                        call.to_string(),
                        KIND_TOOL_CALL_FINISHED,
                        KIND_TOOL_CALL_INTERRUPTED,
                    ),
                    |row| row.get(0),
                )
                .map_err(|error| StorageError::Query(error.to_string()))?;
            if !started || closed {
                return Err(StorageError::NotFound(format!(
                    "unfinished tool call {call} in session {session}"
                )));
            }
            insert_step(
                &tx,
                session,
                &Step::ToolCallInterrupted {
                    call_id: call,
                    partial_output,
                },
            )?;
            tx.commit()
                .map_err(|error| StorageError::Query(error.to_string()))
        })
    }

    fn delete(&self, session: SessionId) -> impl Future<Output = Result<(), StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            session_exists(conn, session)?;
            // 级联删除整个子树：父子从属关系不允许悬挂
            for (table, key) in [
                ("steps", "session_id"),
                ("messages", "session_id"),
                ("sessions", "id"),
            ] {
                let sql = format!("DELETE FROM {table} WHERE {key} IN ({SUBTREE})");
                conn.execute(&sql, [session.to_string()])
                    .map_err(|error| StorageError::Query(error.to_string()))?;
            }
            Ok(())
        })
    }

    fn set_state(
        &self,
        session: SessionId,
        state: SessionState,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            let updated = conn
                .execute(
                    "UPDATE sessions SET state = ?1 WHERE id = ?2",
                    (state_code(state), session.to_string()),
                )
                .map_err(|error| StorageError::Query(error.to_string()))?;
            if updated == 0 {
                return Err(StorageError::NotFound(format!("session {session}")));
            }
            Ok(())
        })
    }

    fn claim_session(
        &self,
        session: SessionId,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            let updated = conn
                .execute(
                    "UPDATE sessions SET state = ?1 WHERE id = ?2 AND state = ?3",
                    (
                        state_code(SessionState::Running),
                        session.to_string(),
                        state_code(SessionState::Pending),
                    ),
                )
                .map_err(|error| StorageError::Query(error.to_string()))?;
            Ok(updated > 0)
        })
    }

    fn session_ids_in_state(
        &self,
        state: SessionState,
    ) -> impl Future<Output = Result<Vec<SessionId>, StorageError>> + Send {
        blocking(&self.conn, move |conn| {
            let mut statement = conn
                .prepare("SELECT id FROM sessions WHERE state = ?1 ORDER BY rowid ASC")
                .map_err(|error| StorageError::Query(error.to_string()))?;
            let rows = statement
                .query_map([state_code(state)], |row| row.get::<_, String>(0))
                .map_err(|error| StorageError::Query(error.to_string()))?;
            let mut ids = Vec::new();
            for row in rows {
                let id = row.map_err(|error| StorageError::Query(error.to_string()))?;
                ids.push(parse_id(&id)?);
            }
            Ok(ids)
        })
    }
}
