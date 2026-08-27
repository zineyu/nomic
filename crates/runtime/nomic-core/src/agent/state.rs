//! agent 状态的共享只读视图（ADR-0035）：查询不走 actor 命令邮箱。
//!
//! actor（ADR-0022）的命令邮箱严格串行：run 类命令（prompt / compact /
//! continue）把整轮 agent loop 包进一条命令，运行期间邮箱不再被消费。
//! 查询若同样走邮箱，运行中的一切状态查询（web `get_state` 快照、
//! supervisor 状态）都会排队到 run 结束——前端 30s 请求超时即表现为
//! 「无法切回活跃会话」。
//!
//! 视图由 agent 本体在状态变更点同步维护（单写者，无并发写竞态）：
//!
//! - 消息落史（与 `MessageEnd` 事件同一点）增量推入；
//! - 历史整体替换 / 弹出（压缩、restore、清空、continue 弹出失败尾）
//!   全量重同步；
//! - 模型 / 系统提示词 / 思考级别 / 统计变更只同步元信息。
//!
//! [`AgentHandle`](crate::AgentHandle) 的查询方法直接读视图（快照隔离）：
//! 运行中即时返回「最后一次应用的状态」，不保证读到仍在邮箱排队的
//! 变更——需要读己之写时先经 `AgentHandle::flush` 屏障同步。
//!
//! 本模块同时收容「历史变更」三方法（`clear_messages` /
//! `restore_messages` / `inject_user_message`）与 `emit_message_end`：
//! 它们与视图同步是同一处不变量，放在一起避免两处漂移（同时缓解
//! `mod.rs` 的行数封顶）。

use std::sync::{Arc, RwLock};

use nomic_ai::{Message, Model, ThinkingLevel, UserMessage, UserMessageContent, now_millis};

use super::{Agent, AgentConfig, AgentEvent, SessionStats};
use crate::compaction::estimate_context_tokens;

/// agent 状态的只读视图（查询快照）：字段与 handle 查询方法一一对应。
#[derive(Debug, Clone)]
pub struct StateView {
    /// 消息历史（最近一次应用的状态）
    pub messages: Vec<Message>,
    /// 当前模型
    pub model: Model,
    /// 当前系统提示词
    pub system_prompt: String,
    /// 当前思考级别
    pub reasoning: Option<ThinkingLevel>,
    /// 上下文 token 估算（与自动压缩同一口径，消息落史 / 压缩时更新）
    pub context_tokens: u64,
    /// 会话统计信息
    pub stats: SessionStats,
}

/// 视图的共享句柄：agent 本体（唯一写者）与各 [`crate::AgentHandle`]（读者）
/// 各持一份；锁内只有短临界区（推入 / 克隆），跨 await 不持锁。
pub type SharedStateView = Arc<RwLock<StateView>>;

/// 以 agent 的初始状态构建共享视图（历史来自 resume 等，token 估算一次）。
pub fn shared_view(
    config: &AgentConfig,
    messages: &[Message],
    system_prompt: &str,
) -> SharedStateView {
    Arc::new(RwLock::new(StateView {
        messages: messages.to_vec(),
        model: config.model.clone(),
        system_prompt: system_prompt.to_string(),
        reasoning: config.stream_options.reasoning,
        context_tokens: estimate_context_tokens(messages),
        stats: SessionStats::default(),
    }))
}

impl Agent {
    /// 视图增量更新：一条消息已落史（与 `MessageEnd` 事件同一点调用，
    /// 附带同一口径的上下文 token 估算，避免重复计算）。
    pub(super) fn view_note_message(&self, message: &Message, context_tokens: u64) {
        let mut view = self.state_view.write().expect("state view lock");
        view.messages.push(message.clone());
        view.context_tokens = context_tokens;
        view.stats = self.stats.clone();
    }

    /// 视图全量重同步：历史整体替换 / 弹出后调用。
    pub(super) fn view_resync(&self) {
        let mut view = self.state_view.write().expect("state view lock");
        *view = StateView {
            messages: self.messages.clone(),
            model: self.config.model.clone(),
            system_prompt: self.system_prompt.clone(),
            reasoning: self.config.stream_options.reasoning,
            context_tokens: estimate_context_tokens(&self.messages),
            stats: self.stats.clone(),
        };
    }

    /// 视图元信息同步：模型 / 系统提示词 / 思考级别 / 统计变更（消息与
    /// token 不变）。
    pub(super) fn view_sync_meta(&self) {
        let mut view = self.state_view.write().expect("state view lock");
        view.model = self.config.model.clone();
        view.system_prompt.clone_from(&self.system_prompt);
        view.reasoning = self.config.stream_options.reasoning;
        view.stats = self.stats.clone();
    }

    /// 清空消息历史（交互端「开启新对话」语义，如 TUI 的 `/new`）。
    ///
    /// 系统提示词、工具与配置保留；应在非运行状态（`prompt` 返回后）调用。
    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.view_resync();
    }

    /// 以既有消息历史整体替换当前上下文（session resume 语义，如 TUI 的 `/resume`）。
    ///
    /// 与 builder 的 `messages` 同样的调用契约：`messages` 按序作为上下文起点，
    /// 调用方负责保证顺序与来源（如 session store 的 `load_messages` 输出）。
    /// 静默替换，不发出事件（历史已在来源 session 渲染/落库）；
    /// 应在非运行状态（`prompt` 返回后）调用。
    pub fn restore_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
        self.view_resync();
    }

    /// 在两轮 prompt 之间向历史注入一条 user 消息（手动载入 skill、外部指令等）。
    ///
    /// 与 [`Self::clear_messages`] 同样的调用契约：仅在非运行状态
    /// （`prompt` 返回后）调用。会发出 `MessageStart`/`MessageEnd` 事件，
    /// 交互端渲染与 session 落库经既有事件管线自动生效。
    pub fn inject_user_message(&mut self, text: &str) {
        let user = Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_string()),
            timestamp: now_millis(),
        });
        self.emit(AgentEvent::MessageStart(Box::new(user.clone())));
        self.messages.push(user.clone());
        self.emit_message_end(user);
    }

    /// 发出 `MessageEnd`：调用方保证消息已落史，事件附带落史后的权威
    /// 上下文估算（锚点规则唯一定义在 `estimate_context_tokens`，
    /// 交互端只抄不算）；查询视图在同一时刻增量同步。
    pub(super) fn emit_message_end(&self, message: Message) {
        let context_tokens = estimate_context_tokens(&self.messages);
        self.view_note_message(&message, context_tokens);
        self.emit(AgentEvent::MessageEnd {
            context_tokens,
            message: Box::new(message),
        });
    }
}
