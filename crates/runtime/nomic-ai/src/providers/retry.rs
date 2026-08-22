//! provider 请求失败时的自动重试。
//!
//! 重试边界受 [`crate::stream`] 事件契约约束：一旦 `Start` 及内容 delta
//! 已推给消费者，透明重试会产生重复事件。因此**只有流建立之前的失败**
//! （连接失败、超时、HTTP 错误状态码）才可重试——此时 `tx` 未发出任何
//! 事件、`output` 未被修改，重试完全安全；流中途的错误（SSE 解析失败、
//! 缺失 `finish_reason` 等）一律按致命错误终止，不重试。
//!
//! 语义：[`RetryPolicy::max_retries`] 为首次失败后允许的**追加**重试次数
//! （默认 3，即最多 4 次尝试）；全部失败后以最后一个错误终止流。

use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// 重试策略：指数退避 `base_delay * 2^(n-1)`（默认 500ms / 1s / 2s）。
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// 首次失败后允许的最大重试次数
    pub max_retries: u32,
    /// 退避基数（第 1 次重试的等待时长）
    pub base_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
        }
    }
}

impl RetryPolicy {
    /// 第 `retry` 次重试（从 1 计）前的等待时长。
    pub const fn delay(&self, retry: u32) -> Duration {
        // 2^(retry-1)；shift 溢出（不现实的重试次数）时退化为基数
        let factor = match 1u32.checked_shl(retry.saturating_sub(1)) {
            Some(factor) => factor,
            None => 1,
        };
        self.base_delay.saturating_mul(factor)
    }
}

/// 一次请求尝试的失败，携带是否可重试的判定。
#[derive(Debug)]
pub struct RequestError {
    /// 错误描述（透传到终止事件的 `error_message`）
    pub message: String,
    /// 是否为可重试的瞬时错误
    pub retryable: bool,
}

impl RequestError {
    /// 构造致命错误（流中错误、API key 缺失、取消等）。
    pub const fn fatal(message: String) -> Self {
        Self {
            message,
            retryable: false,
        }
    }

    /// 传输层错误分类：连接失败与超时为瞬时错误，可重试。
    pub fn from_reqwest(error: &reqwest::Error) -> Self {
        let retryable = error.is_connect() || error.is_timeout();
        tracing::debug!(error = %error, retryable, "request error classified");
        Self {
            message: format!("request failed: {error}"),
            retryable,
        }
    }

    /// HTTP 状态码分类：408 / 429 / 5xx（含 529 overloaded）可重试，
    /// 其余 4xx 为请求本身的确定性错误，重试无意义。
    pub fn from_status(status: reqwest::StatusCode, body: &str) -> Self {
        let retryable = status == reqwest::StatusCode::REQUEST_TIMEOUT
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error();
        tracing::debug!(status = %status, retryable, body_len = body.len(), "HTTP error classified");
        Self {
            message: format!("HTTP {status}: {body}"),
            retryable,
        }
    }
}

/// 睡眠 `delay`，期间响应取消；被取消时返回 `true`。
pub async fn sleep_or_cancel(delay: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => true,
        () = tokio::time::sleep(delay) => false,
    }
}

#[cfg(test)]
pub mod test_server;
#[cfg(test)]
mod tests;
