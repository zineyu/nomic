//! 重试策略与错误分类的单元测试。

use std::time::Duration;

use reqwest::StatusCode;
use tokio_util::sync::CancellationToken;

use super::*;

#[test]
fn backoff_is_exponential() {
    let policy = RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_millis(500),
    };
    assert_eq!(policy.delay(1), Duration::from_millis(500));
    assert_eq!(policy.delay(2), Duration::from_secs(1));
    assert_eq!(policy.delay(3), Duration::from_secs(2));
}

#[test]
fn default_policy_retries_three_times() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_retries, 3);
}

#[test]
fn status_classification() {
    let retryable = [
        StatusCode::REQUEST_TIMEOUT,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::BAD_GATEWAY,
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::GATEWAY_TIMEOUT,
    ];
    for status in retryable {
        assert!(
            RequestError::from_status(status, "").retryable,
            "{status} should be retryable"
        );
    }

    let fatal = [
        StatusCode::BAD_REQUEST,
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::NOT_FOUND,
    ];
    for status in fatal {
        assert!(
            !RequestError::from_status(status, "").retryable,
            "{status} should be fatal"
        );
    }
}

#[test]
fn status_message_format() {
    let error = RequestError::from_status(StatusCode::TOO_MANY_REQUESTS, "slow down");
    assert_eq!(error.message, "HTTP 429 Too Many Requests: slow down");
}

#[tokio::test]
async fn sleep_completes_without_cancel() {
    let cancel = CancellationToken::new();
    let cancelled = sleep_or_cancel(Duration::from_millis(1), &cancel).await;
    assert!(!cancelled);
}

#[tokio::test]
async fn sleep_is_interrupted_by_cancel() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let cancelled = sleep_or_cancel(Duration::from_mins(1), &cancel).await;
    assert!(cancelled);
}
