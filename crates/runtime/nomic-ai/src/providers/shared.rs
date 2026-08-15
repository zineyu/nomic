//! 两个 provider 共用的内部工具与流式请求骨架。

use std::future::Future;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;

use super::retry::{RequestError, RetryPolicy, sleep_or_cancel};
use crate::stream::{AssistantStream, channel};
use crate::types::{AssistantMessage, Model, StopReason};
use crate::{AssistantEvent, now_millis};

/// 空的 assistant 输出消息：流式累积的起点（重试边界的防御性重置也用它）。
pub(super) fn empty_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        content: Vec::new(),
        api: model.api,
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        usage: crate::types::Usage::default(),
        stop_reason: StopReason::Stop,
        error_message: None,
        timestamp: now_millis(),
    }
}

/// 解析流式累积的 partial JSON；被截断时做最大努力修复（未闭合的括号/引号）。
pub(super) fn parse_streaming_json(partial: &str) -> serde_json::Value {
    if let Ok(value) = serde_json::from_str(partial) {
        return value;
    }
    // 截断修复：补齐未闭合的字符串与括号
    let mut fixed = partial.trim_end().to_string();
    let mut in_string = false;
    let mut escaped = false;
    let mut stack = Vec::new();
    for c in fixed.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' | '[' if !in_string => stack.push(c),
            '}' | ']' if !in_string => {
                stack.pop();
            }
            _ => {}
        }
    }
    if in_string {
        fixed.push('"');
    }
    // 去掉可能被截断的尾键值
    while let Some(c) = fixed.chars().last() {
        if c == '{' || c == '[' || c == '"' || c == '}' || c == ']' {
            break;
        }
        fixed.pop();
    }
    if fixed.trim_end().ends_with(':') {
        fixed.push_str("null");
    }
    while let Some(open) = stack.pop() {
        fixed.push(if open == '{' { '}' } else { ']' });
    }
    serde_json::from_str(&fixed)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
}

/// 一次流式请求尝试：构造并发送 HTTP 请求、消费 SSE 流，把增量写入
/// `output` 并经 `tx` 发出事件。两个 provider 各自实现（协议细节不同），
/// 重试与终止（Done/Error）语义统一收在 [`spawn_stream`]。
pub(super) trait StreamAttempt: Send + 'static {
    /// 执行一次请求尝试；`Err` 中标注是否可重试（流建立前的瞬时错误）。
    fn run<'a>(
        &'a mut self,
        output: &'a mut AssistantMessage,
        tx: &'a tokio::sync::mpsc::UnboundedSender<AssistantEvent>,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<(), RequestError>> + Send + 'a;
}

/// 流式请求的共用骨架：按重试策略反复执行 `attempt`（只重试流建立前的
/// 瞬时错误，见 retry 模块文档）；成功后计算成本并发 `Done`，失败
/// （或取消）以 `Error` 终止（取消标 `Aborted`）。
pub(super) fn spawn_stream<A: StreamAttempt>(
    model: &Model,
    retry_policy: RetryPolicy,
    cancel: CancellationToken,
    mut attempt: A,
) -> AssistantStream {
    let (tx, stream) = channel();
    let mut output = empty_output(model);
    let model_for_cost = model.clone();
    let span = tracing::info_span!(
        "llm_request",
        provider = %model.provider,
        model = %model.id,
        base_url = %model.base_url,
    );

    tokio::spawn(
        async move {
            let started = Instant::now();
            let mut retries = 0u32;
            let result = loop {
                let Err(error) = attempt.run(&mut output, &tx, cancel.clone()).await else {
                    break Ok(());
                };
                // 只重试流建立前的瞬时错误（见 retry 模块文档）；
                // 取消与致命错误直接终止
                if !error.retryable || retries >= retry_policy.max_retries || cancel.is_cancelled()
                {
                    break Err(error.message);
                }
                retries += 1;
                let delay = retry_policy.delay(retries);
                tracing::warn!(
                    error = %error.message,
                    retry = retries,
                    max_retries = retry_policy.max_retries,
                    delay_ms = delay.as_millis(),
                    "llm request failed, retrying"
                );
                if sleep_or_cancel(delay, &cancel).await {
                    break Err("request aborted".to_string());
                }
                // 防御性重置：重试边界保证失败时未发出任何事件，
                // output 应未被触碰；重置使该不变式显式成立
                output = empty_output(&model_for_cost);
            };
            let elapsed_ms = started.elapsed().as_millis();
            match result {
                Ok(()) => {
                    model_for_cost.calculate_cost(&mut output.usage);
                    tracing::debug!(
                        stop_reason = ?output.stop_reason,
                        input_tokens = output.usage.input,
                        output_tokens = output.usage.output,
                        cache_read_tokens = output.usage.cache_read,
                        elapsed_ms,
                        "llm request finished"
                    );
                    let _ = tx.send(AssistantEvent::Done {
                        message: Box::new(output),
                    });
                }
                Err(error) => {
                    model_for_cost.calculate_cost(&mut output.usage);
                    if cancel.is_cancelled() {
                        tracing::debug!(elapsed_ms, "llm request aborted");
                    } else {
                        tracing::warn!(%error, elapsed_ms, "llm request failed");
                    }
                    output.stop_reason = if cancel.is_cancelled() {
                        StopReason::Aborted
                    } else {
                        StopReason::Error
                    };
                    output.error_message = Some(error);
                    let _ = tx.send(AssistantEvent::Error {
                        message: Box::new(output),
                    });
                }
            }
        }
        .instrument(span),
    );

    stream
}
