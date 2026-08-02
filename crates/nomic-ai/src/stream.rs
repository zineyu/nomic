//! 流式协议与 provider 抽象。
//!
//! 与 pi 的关键差异（ADR-0001）：delta 事件**不携带** partial 消息快照，
//! 只携带 `(index, delta)` 增量；完整 [`AssistantMessage`] 由终止事件
//! （`Done` / `Error`）一次性交付。需要 partial 状态的消费方自行累积增量。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::types::{AssistantMessage, Context, Model, ThinkingLevel, ToolCall};

/// assistant 响应的流式事件。
///
/// 事件序列契约：`Start` → 任意数量的内容块事件
/// （`XxxStart` → `XxxDelta`* → `XxxEnd`，按内容块交错）→ 恰一个终止事件
/// （`Done` 或 `Error`）。
#[derive(Debug, Clone, PartialEq)]
pub enum AssistantEvent {
    /// 流建立，响应开始
    Start,
    /// 文本块开始（`index` 为内容块在最终消息中的序号）
    TextStart {
        /// 内容块序号
        index: usize,
    },
    /// 文本增量
    TextDelta {
        /// 内容块序号
        index: usize,
        /// 文本增量
        delta: String,
    },
    /// 文本块结束
    TextEnd {
        /// 内容块序号
        index: usize,
    },
    /// 思考块开始
    ThinkingStart {
        /// 内容块序号
        index: usize,
    },
    /// 思考增量
    ThinkingDelta {
        /// 内容块序号
        index: usize,
        /// 思考增量
        delta: String,
    },
    /// 思考块结束
    ThinkingEnd {
        /// 内容块序号
        index: usize,
    },
    /// 工具调用块开始
    ToolCallStart {
        /// 内容块序号
        index: usize,
    },
    /// 工具调用参数增量（partial JSON 片段）
    ToolCallDelta {
        /// 内容块序号
        index: usize,
        /// partial JSON 增量
        delta: String,
    },
    /// 工具调用块结束（参数已解析完毕）
    ToolCallEnd {
        /// 内容块序号
        index: usize,
        /// 完整的工具调用
        tool_call: ToolCall,
    },
    /// 正常终止，`message.stop_reason` ∈ `Stop` | `Length` | `ToolUse`
    Done {
        /// 完整的 assistant 消息
        message: Box<AssistantMessage>,
    },
    /// 异常终止，`message.stop_reason` ∈ `Error` | `Aborted`，
    /// `message.error_message` 携带错误描述
    Error {
        /// 终止时的（可能不完整的）assistant 消息
        message: Box<AssistantMessage>,
    },
}

/// 流式请求选项。
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    /// 采样温度
    pub temperature: Option<f64>,
    /// 最大输出 token 数（缺省用模型的 `max_tokens`）
    pub max_tokens: Option<u64>,
    /// 推理级别（仅 `model.reasoning == true` 时生效）
    pub reasoning: Option<ThinkingLevel>,
    /// API key（缺省从 provider 配置或环境变量解析）
    pub api_key: Option<String>,
    /// 额外请求头（覆盖 provider 默认值）
    pub headers: Vec<(String, String)>,
    /// 请求超时（毫秒）
    pub timeout_ms: Option<u64>,
}

/// provider 违反流协议：流在未发出 `Done` / `Error` 终止事件前关闭。
///
/// 消费方（[`AssistantStream::result`] / [`AssistantStream::result_with`]）
/// 将其统一报告为 `Err`，绝不 panic。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("stream closed without Done/Error")]
pub struct StreamContractError;

/// assistant 事件流的接收端。
///
/// 保证以恰一个 `Done` 或 `Error` 事件收尾，之后流关闭。
/// 终止事件即最终结果，无需额外的 result handle（与 pi 的
/// `EventStream.result()` 相比更线性）。
pub struct AssistantStream {
    rx: mpsc::UnboundedReceiver<AssistantEvent>,
}

impl AssistantStream {
    /// 接收下一个事件；流在 `Done` / `Error` 之后关闭（返回 `None`）。
    pub async fn next(&mut self) -> Option<AssistantEvent> {
        self.rx.recv().await
    }

    /// 消费流直到终止事件，返回最终的 [`AssistantMessage`]。
    ///
    /// 终止事件前的中间事件（`Start` 与各 delta）按到达顺序以值传给
    /// `on_event`；provider 违约（流未以 `Done` / `Error` 收尾即关闭）
    /// 时返回 [`StreamContractError`]。
    pub async fn result_with(
        mut self,
        mut on_event: impl FnMut(AssistantEvent),
    ) -> Result<AssistantMessage, StreamContractError> {
        let mut final_message = None;
        while let Some(event) = self.next().await {
            match event {
                AssistantEvent::Done { message } | AssistantEvent::Error { message } => {
                    final_message = Some(*message);
                }
                event => on_event(event),
            }
        }
        final_message.ok_or(StreamContractError)
    }

    /// 消费流直到终止事件，返回最终的 [`AssistantMessage`]，丢弃中间事件。
    pub async fn result(self) -> Result<AssistantMessage, StreamContractError> {
        self.result_with(|_| {}).await
    }
}

impl std::fmt::Debug for AssistantStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantStream").finish_non_exhaustive()
    }
}

/// 供 provider 实现者构造事件流：返回 (发送端, 接收端)。
///
/// provider 在后台任务中经发送端推送事件。丢弃发送端即关闭流——若此时
/// 尚未发出 `Done` / `Error`，即构成契约违约，消费方 fold 时会收到
/// [`StreamContractError`]。
pub fn channel() -> (mpsc::UnboundedSender<AssistantEvent>, AssistantStream) {
    let (tx, rx) = mpsc::unbounded_channel();
    (tx, AssistantStream { rx })
}

/// provider 的统一流式契约。
///
/// **错误契约**（与 pi 一致）：请求/运行时失败**不返回 `Err`**，
/// 而是编码为终止事件 [`AssistantEvent::Error`]；只有立即可判定的
/// 编程错误（如无法构造 HTTP 客户端）才允许返回 `Err`。
/// 取消通过 [`CancellationToken`] 表达，取消时流以
/// `stop_reason = Aborted` 的 `Error` 事件收尾。
///
/// 若实现违反事件序列契约（流未以 `Done` / `Error` 收尾即关闭），消费方
/// 统一收到 [`StreamContractError`]，绝不 panic。
pub trait Provider: Send + Sync {
    /// 发起一次流式请求，立即返回事件流（网络交互在后台任务中进行）。
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
        cancel: CancellationToken,
    ) -> AssistantStream;
}
