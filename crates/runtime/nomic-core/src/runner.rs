//! session runner：[`AgentHandle`] 之上的「会话级串行运行」语义。
//!
//! actor（ADR-0022）保证单命令的串行执行与取消，交互端（TUI driver、
//! web runner）还需要一层「job」语义：prompt / 压缩 / 续跑共用一个串行
//! 队列，每个 job 携带独立取消令牌，执行结果与生命周期经事件流回传。
//! 这层语义曾由两个交互端各自手写（web 侧还需锁 + 原子量规避
//! 「出队发现队列空但 submit 已入队」的丢单竞态），现收敛到本模块：
//!
//! - [`SessionJob`]：run 类 job 枚举（Prompt / Compact / Continue）；
//!   注入、清空、模型切换等 fire-and-forget 变更不是 job，直接调
//!   [`AgentHandle`]（邮箱 FIFO 保证其先于紧随的 job 生效）；
//! - 串行消费：内部单任务经 mpsc 队列按提交顺序执行 job，天然无丢单
//!   竞态（队列即 channel，不存在「队列+标志」两份状态）；
//! - 取消：每个 job 在开始执行时获得独立 [`CancellationToken`]，
//!   [`SessionRunner::cancel_current`] 只中断在途 job，排队 job 保留；
//! - 空结果就地通知：compact / continue 的空结果（无可压缩内容、历史
//!   尾部无可续跑消息）不产生 agent 事件，翻译为 [`CompactOutcome`] /
//!   [`ContinueOutcome`] 的专设变体，通知文案常量化（交互端同一口径）；
//! - 生命周期翻译：compact 不产生 `AgentStart`/`AgentEnd`，runner 对
//!   全部 job 统一发射 [`RunnerEvent::Started`] / [`RunnerEvent::Finished`]，
//!   交互端据此合成运行生命周期（web 的 RunStarted/RunFinished 等）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use nomic_ai::{ImageContent, Message, StopReason};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::{ActorError, AgentHandle};
use crate::compaction::Compaction;

/// 无可压缩内容的就地通知文案（交互端共享同一口径）。
pub const NOTHING_TO_COMPACT: &str = "上下文很短，没有可压缩的内容。";

/// 历史尾部无可续跑消息的就地通知文案（交互端共享同一口径）。
pub const NOTHING_TO_CONTINUE: &str = "历史尾部没有可续跑的消息。";

/// 提交给 runner 的 run 类 job：prompt / 手动压缩 / 续跑。
///
/// 共用同一队列串行执行：运行中提交的 job 排队，在途 job 完成后按序
/// 执行，不与在途运行并发。
#[derive(Debug)]
pub enum SessionJob {
    /// 运行一轮 prompt（mention 等展开已由调用方完成）
    Prompt {
        /// prompt 文本
        text: String,
        /// 图片附件
        images: Vec<ImageContent>,
    },
    /// 手动压缩上下文（`/compact [聚焦指令]` 语义）
    Compact {
        /// 聚焦指令
        instructions: Option<String>,
    },
    /// 续跑：重发历史尾部的消息（user 消息或 tool result）
    Continue,
}

/// job 种类（生命周期事件用；不含载荷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    /// prompt 运行
    Prompt,
    /// 手动压缩
    Compact,
    /// 续跑
    Continue,
}

impl SessionJob {
    /// job 的种类（去载荷）。
    pub const fn kind(&self) -> JobKind {
        match self {
            Self::Prompt { .. } => JobKind::Prompt,
            Self::Compact { .. } => JobKind::Compact,
            Self::Continue => JobKind::Continue,
        }
    }
}

/// runner 发射的事件：job 生命周期与执行结果。
///
/// 每个 job 严格按序产生 `Started` → `Finished` 各一条（执行失败同样
/// 收尾 `Finished`，错误在 [`JobOutcome`] 内）。prompt / continue 的
/// agent 事件流自带 `AgentStart`/`AgentEnd`，compact 没有——需要运行
/// 生命周期的交互端应以本事件为准（至少对 compact）。
#[derive(Debug)]
pub enum RunnerEvent {
    /// 一个 job 出队、开始执行（附种类）
    Started(JobKind),
    /// 一个 job 执行结束（附结果；Err 为 loop / 压缩失败）
    Finished(JobOutcome),
}

/// 一个 job 的执行结果，按种类分派。
#[derive(Debug)]
pub enum JobOutcome {
    /// prompt 运行结束
    Prompt(Result<PromptOutcome, ActorError>),
    /// 手动压缩结束
    Compact(Result<CompactOutcome, ActorError>),
    /// 续跑结束
    Continue(Result<ContinueOutcome, ActorError>),
}

/// 一轮 prompt 的结束结果（goal 追问等「是否正常结束」判定的依据）。
#[derive(Debug)]
pub struct PromptOutcome {
    /// 是否正常结束：本轮被取消（用户中断）或响应以 Error/Aborted
    /// 收尾时为 false——失败与中断的恢复由用户主导
    ended_normally: bool,
}

impl PromptOutcome {
    /// 本轮是否正常结束（未被取消且未以 Error/Aborted 收尾）。
    pub const fn ended_normally(&self) -> bool {
        self.ended_normally
    }
}

/// 一次手动压缩的结束结果。
#[derive(Debug)]
pub enum CompactOutcome {
    /// 压缩完成（结果经 `CompactionStart`/`CompactionEnd` 事件渲染与落库，
    /// 交互端通常无需再处理）
    Compacted(Compaction),
    /// 无可压缩内容：不产生 agent 事件，交互端按 [`NOTHING_TO_COMPACT`]
    /// 就地通知
    NothingToCompact,
}

/// 一次续跑的结束结果。
#[derive(Debug)]
pub enum ContinueOutcome {
    /// 已续跑（经 agent 事件流渲染与落库，交互端通常无需再处理）
    Continued,
    /// 历史尾部没有可续跑的消息：不产生 agent 事件，交互端按
    /// [`NOTHING_TO_CONTINUE`] 就地通知
    NothingToContinue,
}

/// runner 调用错误：runner 任务已退出（全部句柄断开后自然退出或 panic）。
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    /// runner 任务已退出：job 入队失败
    #[error("session runner 已退出")]
    Gone,
}

/// runner 内部共享状态：在途取消令牌与队列/运行标志（快照查询用）。
///
/// 队列即 channel，深度与运行标志仅服务状态快照，不参与调度（调度
/// 正确性由 channel 本身保证，无「队列+标志」双状态竞态）。
#[derive(Debug)]
struct Shared {
    /// 在途 job 的取消令牌（`cancel_current` 用）；job 开始执行时放置，
    /// 结束时取走。提交时若槽位为空也会预放（覆盖「已提交未开始」的
    /// 取消窗口）；排队中的 job 不占槽位——取消只作用于在途 job
    current: Mutex<Option<CancellationToken>>,
    /// 已提交未出队的 job 数（状态快照用）
    queued: AtomicUsize,
    /// 是否有 job 正在执行（状态快照用）
    executing: AtomicBool,
}

/// session runner 句柄：提交 run 类 job、取消在途 job、查询队列状态。
///
/// 由 [`SessionRunner::spawn`] 创建；事件流接收端在 spawn 时取得。
/// 全部句柄断开后 runner 任务退出，事件通道随之关闭。
#[derive(Debug)]
pub struct SessionRunner {
    job_tx: mpsc::UnboundedSender<(SessionJob, CancellationToken)>,
    shared: Arc<Shared>,
}

impl SessionRunner {
    /// 启动 session runner：串行消费 job 队列，经 [`AgentHandle`] 执行。
    ///
    /// 返回句柄、事件流接收端与任务的 `JoinHandle`（任务 panic 经
    /// `JoinHandle` 暴露详情，事件通道随之关闭）。
    pub fn spawn(
        handle: AgentHandle,
    ) -> (
        Self,
        mpsc::UnboundedReceiver<RunnerEvent>,
        tokio::task::JoinHandle<()>,
    ) {
        let (job_tx, mut job_rx) = mpsc::unbounded_channel::<(SessionJob, CancellationToken)>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<RunnerEvent>();
        let shared = Arc::new(Shared {
            current: Mutex::new(None),
            queued: AtomicUsize::new(0),
            executing: AtomicBool::new(false),
        });
        let task_shared = shared.clone();
        let task = tokio::spawn(async move {
            while let Some((job, token)) = job_rx.recv().await {
                task_shared.queued.fetch_sub(1, Ordering::SeqCst);
                *task_shared.current.lock().expect("lock") = Some(token.clone());
                task_shared.executing.store(true, Ordering::SeqCst);
                if event_tx.send(RunnerEvent::Started(job.kind())).is_err() {
                    return;
                }
                let outcome = run_job(&handle, job, &token).await;
                task_shared.executing.store(false, Ordering::SeqCst);
                task_shared.current.lock().expect("lock").take();
                if event_tx.send(RunnerEvent::Finished(outcome)).is_err() {
                    return;
                }
            }
        });
        (Self { job_tx, shared }, event_rx, task)
    }

    /// 提交一个 job；串行执行（运行中排队，在途完成后按序执行）。
    ///
    /// 每个 job 获得独立取消令牌（开始执行时成为「在途」）。提交时若
    /// 无在途 job，令牌预放入在途槽位，覆盖「已提交未开始」的取消
    /// 窗口。
    pub fn submit(&self, job: SessionJob) -> Result<(), RunnerError> {
        let token = CancellationToken::new();
        {
            let mut current = self.shared.current.lock().expect("lock");
            if current.is_none() {
                *current = Some(token.clone());
            }
        }
        self.shared.queued.fetch_add(1, Ordering::SeqCst);
        if self.job_tx.send((job, token)).is_err() {
            self.shared.queued.fetch_sub(1, Ordering::SeqCst);
            self.shared.current.lock().expect("lock").take();
            return Err(RunnerError::Gone);
        }
        Ok(())
    }

    /// 取消在途 job；没有在途运行时返回 `false`（排队 job 保留）。
    pub fn cancel_current(&self) -> bool {
        self.shared
            .current
            .lock()
            .expect("lock")
            .take()
            .is_some_and(|token| {
                token.cancel();
                true
            })
    }

    /// 已提交未出队的 job 数（状态快照用）。
    pub fn queued_len(&self) -> usize {
        self.shared.queued.load(Ordering::SeqCst)
    }

    /// 是否有 job 在途或排队（状态快照的「运行中」口径）。
    pub fn is_running(&self) -> bool {
        self.shared.executing.load(Ordering::SeqCst) || self.queued_len() > 0
    }
}

/// 执行一个 job 并翻译结果：空结果转为专设变体，prompt 汇总
/// 「是否正常结束」判定依据（取消令牌与尾部 stop reason）。
async fn run_job(handle: &AgentHandle, job: SessionJob, token: &CancellationToken) -> JobOutcome {
    match job {
        SessionJob::Prompt { text, images } => {
            let result = handle
                .prompt_with_images(&text, &images, token.clone())
                .await;
            JobOutcome::Prompt(result.map(|messages| PromptOutcome {
                ended_normally: ended_normally(&messages, token),
            }))
        }
        SessionJob::Compact { instructions } => {
            let result = handle.compact(instructions.as_deref(), token.clone()).await;
            JobOutcome::Compact(result.map(|outcome| match outcome {
                Some(compaction) => CompactOutcome::Compacted(compaction),
                None => CompactOutcome::NothingToCompact,
            }))
        }
        SessionJob::Continue => {
            let result = handle.continue_run(token.clone()).await;
            JobOutcome::Continue(result.map(|outcome| match outcome {
                Some(_) => ContinueOutcome::Continued,
                None => ContinueOutcome::NothingToContinue,
            }))
        }
    }
}

/// prompt 是否正常结束：未被取消，且最后一条 assistant 消息不是以
/// Error/Aborted 收尾。
fn ended_normally(messages: &[Message], token: &CancellationToken) -> bool {
    let last_stop = messages.iter().rev().find_map(|message| match message {
        Message::Assistant(assistant) => Some(assistant.stop_reason),
        _ => None,
    });
    !token.is_cancelled() && !matches!(last_stop, Some(StopReason::Error | StopReason::Aborted))
}
