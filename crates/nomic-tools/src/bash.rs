//! `bash` 工具：超时、尾部截断、完整输出落临时文件、100ms 节流进度（契约与 pi 一致）。

use std::mem;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdate, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::truncate::{Continuation, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};

/// 进度更新的节流间隔。
const UPDATE_THROTTLE: Duration = Duration::from_millis(100);

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashParams {
    /// Bash command to execute
    pub command: String,
    /// Timeout in seconds (optional, no default timeout)
    pub timeout: Option<f64>,
}

/// `bash` 工具。
#[derive(Debug, Default, Clone, Copy)]
pub struct BashTool;

const LABEL: &str = "bash";
const DESCRIPTION: &str = "Execute a bash command in the current working directory. Returns stdout and stderr.      Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated,      full output is saved to a temp file. Optionally provide a timeout in seconds.";

#[async_trait]
impl AgentTool for BashTool {
    type Params = BashParams;

    fn name(&self) -> &'static str {
        "bash"
    }

    fn label(&self) -> &str {
        LABEL
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    async fn execute(
        &self,
        params: Self::Params,
        cancel: CancellationToken,
        on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        if let Some(timeout) = params.timeout
            && (!timeout.is_finite() || timeout <= 0.0)
        {
            return Err(ToolError::new(
                "Invalid timeout: must be a finite positive number of seconds",
            ));
        }
        tracing::debug!(command = %params.command, timeout = ?params.timeout, "bash start");
        let started = Instant::now();

        let mut child = tokio::process::Command::new("bash")
            .arg("-c")
            .arg(&params.command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ToolError::new(format!("Could not spawn bash: {e}")))?;

        // stdout/stderr 按到达顺序合并到共享缓冲
        let buffer = Arc::new(Mutex::new(String::new()));
        let mut readers = tokio::task::JoinSet::new();
        readers.spawn(capture_output(
            child.stdout.take().expect("piped stdout"),
            Arc::clone(&buffer),
        ));
        readers.spawn(capture_output(
            child.stderr.take().expect("piped stderr"),
            Arc::clone(&buffer),
        ));

        let (progress_tx, progress_task) = spawn_progress_task(Arc::clone(&buffer), on_update);

        let outcome = wait_for_child(
            &mut child,
            params.timeout.map(Duration::from_secs_f64),
            &cancel,
        )
        .await;

        let exit_code = if let WaitOutcome::Exited(status) = &outcome {
            status
                .as_ref()
                .ok()
                .and_then(std::process::ExitStatus::code)
        } else {
            let _ = child.kill().await;
            let _ = child.wait().await;
            None
        };
        // 等读取任务自然读到 EOF（不能 abort，否则丢弃管道中未读的数据）
        while readers.join_next().await.is_some() {}
        progress_tx.send_modify(|_| {});
        drop(progress_tx);
        let _ = progress_task.await;

        let full_output = mem::take(
            &mut *buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let (output_text, details) = assemble_output(&full_output);

        let append_status = |status_text: &str| {
            if output_text.is_empty() {
                status_text.to_string()
            } else {
                format!("{output_text}\n\n{status_text}")
            }
        };
        match outcome {
            WaitOutcome::Aborted => Err(ToolError::new(append_status("Command aborted"))),
            WaitOutcome::TimedOut => {
                let seconds = params.timeout.unwrap_or_default();
                Err(ToolError::new(append_status(&format!(
                    "Command timed out after {seconds} seconds"
                ))))
            }
            WaitOutcome::Exited(_) => {
                tracing::debug!(
                    exit_code,
                    elapsed_ms = started.elapsed().as_millis(),
                    output_bytes = full_output.len(),
                    "bash finished"
                );
                if let Some(code) = exit_code
                    && code != 0
                {
                    return Err(ToolError::new(append_status(&format!(
                        "Command exited with code {code}"
                    ))));
                }
                let mut result = ToolResult::text(if output_text.is_empty() {
                    "(no output)"
                } else {
                    &output_text
                });
                result.details = details;
                Ok(result)
            }
        }
    }
}

/// 节流（100ms）推送截断后的进度更新；drop 返回的 sender 即停止。
fn spawn_progress_task(
    buffer: Arc<Mutex<String>>,
    on_update: ToolUpdateCallback,
) -> (
    tokio::sync::watch::Sender<Instant>,
    tokio::task::JoinHandle<()>,
) {
    let (progress_tx, mut progress_rx) = tokio::sync::watch::channel(Instant::now());
    let task = tokio::spawn(async move {
        let mut last = Instant::now()
            .checked_sub(UPDATE_THROTTLE)
            .unwrap_or_else(Instant::now);
        while progress_rx.changed().await.is_ok() {
            let elapsed = last.elapsed();
            if elapsed < UPDATE_THROTTLE {
                tokio::time::sleep(UPDATE_THROTTLE.checked_sub(elapsed).unwrap()).await;
            }
            last = Instant::now();
            let output = buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let truncation = truncate_tail(&output, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
            on_update(ToolUpdate {
                content: vec![nomic_ai::UserContent::Text(nomic_ai::TextContent {
                    text: truncation.content,
                    text_signature: None,
                })],
                details: None,
            });
        }
    });
    (progress_tx, task)
}

/// 等待子进程退出 / 超时 / 取消。
async fn wait_for_child(
    child: &mut tokio::process::Child,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> WaitOutcome {
    tokio::select! {
        biased;
        () = cancel.cancelled() => WaitOutcome::Aborted,
        status = child.wait() => WaitOutcome::Exited(status),
        () = async {
            match timeout {
                Some(t) => tokio::time::sleep(t).await,
                None => std::future::pending().await,
            }
        } => WaitOutcome::TimedOut,
    }
}

/// 命令等待的结局。
enum WaitOutcome {
    Exited(std::io::Result<std::process::ExitStatus>),
    TimedOut,
    Aborted,
}

/// 读取一个输出流并按到达顺序追加到共享缓冲。
async fn capture_output<R>(stream: R, buffer: Arc<Mutex<String>>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = tokio::io::BufReader::new(stream);
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let text = String::from_utf8_lossy(&chunk[..n]);
                buffer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push_str(&text);
            }
        }
    }
}

/// 截断输出并组装展示文本与 details。
fn assemble_output(full_output: &str) -> (String, Option<serde_json::Value>) {
    let truncation = truncate_tail(full_output, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES);
    if !truncation.truncated {
        return (truncation.content, None);
    }
    let full_output_path = save_full_output(full_output);
    let start_line = truncation.total_lines - truncation.output_lines + 1;
    let notice = truncation
        .notice(
            start_line,
            truncation.total_lines,
            &Continuation::FullOutput(full_output_path.clone()),
        )
        .unwrap_or_default();
    let details = serde_json::json!({
        "truncation": { "total_lines": truncation.total_lines, "output_lines": truncation.output_lines },
        "full_output_path": full_output_path,
    });
    (format!("{}\n\n{notice}", truncation.content), Some(details))
}

/// 完整输出写入临时文件，返回路径。
fn save_full_output(output: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = std::env::temp_dir().join(format!("nomic-bash-{nanos}.log"));
    match std::fs::write(&path, output) {
        Ok(()) => path.display().to_string(),
        Err(_) => "(failed to save full output)".to_string(),
    }
}
