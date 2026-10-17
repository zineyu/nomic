//! 存储抽象。
//!
//! server 全库只经本 trait；SQLite 是首个实现（S2），
//! 内部负责 schema 版本迁移。接口为异步，操作对象全部是领域类型，
//! 不泄漏任何具体存储引擎的类型。

use std::future::Future;
use std::path::PathBuf;

use crate::error::StorageError;
use crate::id::{MessageId, SessionId, ToolCallId};
use crate::message::Message;
use crate::session::{Session, SessionState};
use crate::step::Step;
use crate::submission::{Submission, SubmissionRecord};
use crate::tree::SessionTreeSnapshot;

/// 存储抽象：会话树、步骤日志与受理台账的唯一读写入口。
pub trait Storage: Send + Sync {
    /// 受理一次提交：按 `request_id` 去重，首次受理返回 `Accepted`；
    /// 同一 `request_id` 的重复提交返回 `Duplicate`（携带原受理的
    /// 会话 id），消息绝不重复进入会话。
    ///
    /// 目标会话空闲（非运行中）时，用户消息立即排入步骤日志，
    /// 返回值中的 `message` 即该消息（服务端据以广播）。
    /// 目标会话正在运行时（worker 持有、回合进行中），消息先持久
    /// 化为待处理提交，在回合边界由
    /// [`Storage::drain_pending_submissions`] 排入步骤日志；此时
    /// `message` 为 `None`（广播由排入方发出）。重复受理同样为
    /// `None`。
    ///
    /// `target` 为 `New` 时创建新会话（新建会话并非运行中，消息
    /// 立即排入）。台账随存储持久化：进程崩溃后重发的提交仍被
    /// 识别为重复。
    fn record_submission(
        &self,
        submission: &Submission,
    ) -> impl Future<Output = Result<SubmissionRecord, StorageError>> + Send;

    /// 直接创建新会话（系统路径使用，如恢复入口）。
    fn create_session(
        &self,
        working_dir: PathBuf,
    ) -> impl Future<Output = Result<Session, StorageError>> + Send;

    /// 取会话；`None` 表示确实不存在（与故障区分开）。
    fn get_session(
        &self,
        id: SessionId,
    ) -> impl Future<Output = Result<Option<Session>, StorageError>> + Send;

    /// 追加一个已确认步骤（checkpoint 单元）。
    ///
    /// # Errors
    ///
    /// 目标会话不存在时返回 [`StorageError::NotFound`]。
    fn append_step(
        &self,
        session: SessionId,
        step: Step,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// 读取会话的完整步骤序列（回放路径）。
    fn steps(
        &self,
        session: SessionId,
    ) -> impl Future<Output = Result<Vec<Step>, StorageError>> + Send;

    /// 读取会话树快照（只含会话元数据，不含消息体）。
    fn session_tree(
        &self,
    ) -> impl Future<Output = Result<SessionTreeSnapshot, StorageError>> + Send;

    /// 在 `source` 的 `at` 消息处分叉：复制 `source` 截至 `at`（含）的
    /// 完整步骤前缀（消息与它们之间的工具步骤）作为新会话的初始
    /// 序列（写时复制），返回新会话。子会话只在分叉点之后重新起算
    /// 执行日志。`at` 不属于 `source` 时返回 [`StorageError::NotFound`]。
    /// 分叉后父子追加互不可见。
    fn fork(
        &self,
        source: SessionId,
        at: MessageId,
    ) -> impl Future<Output = Result<Session, StorageError>> + Send;

    /// 删除会话及其整个子树（分叉后代）：父子从属关系不允许悬挂，
    /// 删除父会话时后代一并删除。受理台账保留——历史提交的
    /// requestId 去重不受影响。
    ///
    /// # Errors
    ///
    /// 会话不存在时返回 [`StorageError::NotFound`]。
    fn delete(&self, session: SessionId) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// 把未完成的工具调用标记为 interrupted：恢复时其已提交的
    /// 部分输出回灌模型，副作用绝不重复执行。
    ///
    /// `partial_output` 为中断前已提交的输出；无已提交输出时为 `None`。
    /// 已完成的调用不可再标记。
    ///
    /// # Errors
    ///
    /// 目标会话不存在、或调用不存在/已完成时返回
    /// [`StorageError::NotFound`]。
    fn mark_interrupted(
        &self,
        session: SessionId,
        call: ToolCallId,
        partial_output: Option<String>,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// 设置会话执行状态（调度与重驱路径）。
    ///
    /// # Errors
    ///
    /// 会话不存在时返回 [`StorageError::NotFound`]。
    fn set_state(
        &self,
        session: SessionId,
        state: SessionState,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// 运行收尾：原子判定会话是否仍有待处理提交（执行期间受理的）。
    ///
    /// - 无待处理提交：运行中 → 静止，返回 `true`（会话已释放）。
    /// - 有待处理提交：保持运行中，返回 `false`——调用方应继续下一
    ///   回合，先 [`Storage::drain_pending_submissions`] 再请求 LLM。
    ///
    /// 判定与状态转换在同一事务内完成：避免「查无待处理」与「转
    /// 静止」之间到达的提交被静默丢弃。
    ///
    /// # Errors
    ///
    /// 会话不存在时返回 [`StorageError::NotFound`]。
    fn quiesce_session(
        &self,
        session: SessionId,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send;

    /// 回合边界：把当前会话的全部待处理提交按受理顺序排入步骤日志，
    /// 返回排入的用户消息（调用方据以广播，保证事件顺序与步骤一致）。
    ///
    /// 排入与删除待处理记录在同一事务内完成，崩溃后不会重复排入。
    ///
    /// # Errors
    ///
    /// 会话不存在或待处理提交载荷损坏时返回对应错误。
    fn drain_pending_submissions(
        &self,
        session: SessionId,
    ) -> impl Future<Output = Result<Vec<Message>, StorageError>> + Send;

    /// 领取调度请求指定的会话：仅当处于待处理时原子转为运行中。
    /// 会话不存在或不在待处理时返回 `Ok(false)`（幂等跳过）。
    ///
    /// 调度单位是显式请求的会话 id（受理/恢复），不是全局队列：
    /// 失败挂回待处理后的会话不会被无关请求重复领取。
    fn claim_session(
        &self,
        session: SessionId,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send;

    /// 按执行状态枚举会话 id（稳定顺序：创建先后）。
    ///
    /// 启动恢复用它枚举待处理工作；本方法只读，不改变会话状态，
    /// 调度器仍按显式会话 id 原子领取（[`Storage::claim_session`]）。
    fn session_ids_in_state(
        &self,
        state: SessionState,
    ) -> impl Future<Output = Result<Vec<SessionId>, StorageError>> + Send;
}
