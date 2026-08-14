//! 落库策略：消费 [`AgentEvent`] 流，在定稿点落库并推进父指针。
//!
//! 「何时落库、落什么、父指针怎么推进」曾经由 print 与 TUI 两个调用端
//! 各自实现且语义漂移（print 恒 `parent=None` 自动链最新，TUI 自维护
//! tip 推进）。本模块把策略收到 seam（事件流）后面：调用端对每个事件
//! 只做一行 [`SessionRecorder::record`] 接线，落库 bug 集中于本模块，
//! 测试直接打在事件流上（见 `tests/recorder.rs`）。
//!
//! 父指针（tip）语义：当前分支末端的 entry id，`None` 表示自动链到该
//! session 最新 entry（新 session / 重置后）。每次落库成功推进到新
//! entry；失败不推进（下次追加重试同一父指针，store 非权威源，由调用
//! 端决定如何提示）。`/tree` 创建分支即 [`SessionRecorder::set_tip`] 显式
//! 切换；`/new` / `/resume` 经 [`SessionRecorder::switch`] 换目标 session。

use nomic_core::AgentEvent;

use crate::{CompactionRecord, SessionError, SessionStore};

/// 会话落库器：持有 store、目标 session 与父指针，消费 agent 事件流。
#[derive(Debug, Clone)]
pub struct SessionRecorder {
    store: SessionStore,
    session_id: String,
    /// 落库父指针：当前分支末端的 entry id（`None` 时追加自动链到最新
    /// entry——新 session 或重置后的状态）
    tip: Option<String>,
}

impl SessionRecorder {
    /// 新 session 的落库器：尚无条目，父指针 `None`（首次追加自动链最新）。
    pub const fn new(store: SessionStore, session_id: String) -> Self {
        Self {
            store,
            session_id,
            tip: None,
        }
    }

    /// 恢复的 session 的落库器：父指针从默认分支末端起算（分支场景下
    /// 保证续写落在默认分支而非全局最新 entry）。
    pub const fn with_tip(store: SessionStore, session_id: String, tip: Option<String>) -> Self {
        Self {
            store,
            session_id,
            tip,
        }
    }

    /// 底层 store（session 管理操作复用同一连接）。
    pub const fn store(&self) -> &SessionStore {
        &self.store
    }

    /// 目标 session id。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 当前落库父指针（`/tree` 选择器预选中当前分支末端用）。
    pub fn tip(&self) -> Option<&str> {
        self.tip.as_deref()
    }

    /// 显式切换落库父指针（`/tree` 从所选条目创建分支）；`None` 重置为
    /// 自动链最新。
    pub fn set_tip(&mut self, tip: Option<String>) {
        self.tip = tip;
    }

    /// 切换目标 session（`/new` 传 `tip=None` 重置；`/resume` 传恢复的
    /// 分支末端）。
    pub fn switch(&mut self, session_id: String, tip: Option<String>) {
        self.session_id = session_id;
        self.tip = tip;
    }

    /// 消费一个 agent 事件：定稿点（`MessageEnd` / `CompactionEnd`）以
    /// 当前父指针落库，成功后推进父指针；其余事件忽略。
    ///
    /// 失败返回错误且不推进父指针、不中断运行（store 非权威源，提示
    /// 方式由调用端决定）。
    pub async fn record(&mut self, event: &AgentEvent) -> Result<(), SessionError> {
        match event {
            AgentEvent::MessageEnd(message) => {
                let entry_id = self
                    .store
                    .append_message(&self.session_id, self.tip.as_deref(), message)
                    .await?;
                self.tip = Some(entry_id);
            }
            AgentEvent::CompactionEnd {
                summary,
                tokens_before,
                kept_count,
                ..
            } => {
                let record = CompactionRecord {
                    summary: summary.clone(),
                    kept_count: *kept_count as u64,
                    tokens_before: *tokens_before,
                };
                let entry_id = self
                    .store
                    .append_compaction(&self.session_id, self.tip.as_deref(), &record)
                    .await?;
                self.tip = Some(entry_id);
            }
            _ => {}
        }
        Ok(())
    }
}
