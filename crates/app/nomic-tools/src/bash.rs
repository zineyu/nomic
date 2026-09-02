//! `bash` 工具：默认 60s 超时（`timeout` 参数可覆盖，单位同为秒）、超时/取消时
//! 强杀整个进程组并返回已收集输出、尾部截断、完整输出落临时文件、100ms 节流
//! 进度（输出格式契约与 pi 一致）。
//!
//! 超时强杀以进程组为单位：子进程经 `process_group(0)` 成为进程组组长，
//! 超时/取消时 SIGKILL 整个组——只杀 shell 会让前台孙进程（如 `sleep`）存活并
//! 继续持有输出管道，读取任务永远等不到 EOF，超时形同虚设。

use std::collections::HashMap;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdate, ToolUpdateCallback};
use nomic_vfs::VfsRouter;
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::truncate::{Continuation, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncate_tail};

/// 进度更新的节流间隔。
const UPDATE_THROTTLE: Duration = Duration::from_millis(100);

/// 默认超时：60 秒（`timeout` 参数可覆盖，单位同为秒）。
const DEFAULT_TIMEOUT: Duration = Duration::from_mins(1);

/// 强杀后排干输出管道的宽限：进程组被 SIGKILL 后管道立即关闭，
/// 正常只需一次调度；宽限耗尽仍有任务持有管道（孙进程脱离进程组另行
/// `setsid`）则放弃读取，已收集的输出不丢。
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct BashParams {
    /// Bash command to execute
    pub command: String,
    /// Timeout in seconds (optional, defaults to 60). On timeout the process
    /// group is killed and the output collected so far is returned.
    pub timeout: Option<f64>,
}

/// `bash` 工具。
#[derive(Debug, Default, Clone)]
pub struct BashTool {
    /// VFS 挂载表：`cd <uri>` 重写为底层 source_path
    ///（ADR-0040 §9）；`None` 时命令原样执行
    vfs_router: Option<Arc<VfsRouter>>,
    /// 命令执行的基准目录（workspace 严格归属；空句柄 = 进程 cwd）
    base: crate::base::BaseDir,
    /// 会话级 nix 环境缓存（ADR-0041）；`None` 时始终用宿主环境
    nix_env: Option<Arc<crate::nix_env::NixEnvCache>>,
}

impl BashTool {
    /// 创建以进程 cwd 为基准的 bash 工具。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置固定基准目录：命令在该目录下执行（workspace 严格归属）。
    #[must_use]
    pub fn with_base_dir(mut self, base_dir: Option<PathBuf>) -> Self {
        self.base = crate::base::BaseDir::new(base_dir);
        self
    }

    /// 共享基准目录句柄：句柄更新后本工具的下一次执行即用新基准
    ///（交互端切换 session 的 workspace 场景）。
    #[must_use]
    pub fn with_shared_base_dir(mut self, base: &crate::base::BaseDir) -> Self {
        self.base = base.clone();
        self
    }

    /// 挂 VFS 挂载表（会话共享实例）。
    #[must_use]
    pub fn with_vfs_router(mut self, vfs_router: Arc<VfsRouter>) -> Self {
        self.vfs_router = Some(vfs_router);
        self
    }

    /// 挂会话级 nix 环境缓存：命令注入 workspace 的纯净环境执行，
    /// 不可用时回退宿主环境并附尾注（ADR-0041）。
    #[must_use]
    pub fn with_nix_env(mut self, cache: Arc<crate::nix_env::NixEnvCache>) -> Self {
        self.nix_env = Some(cache);
        self
    }

    /// 取本次调用的执行环境：命中缓存返回纯净 env；未启用/未注入返回
    /// `(None, None)`；解析失败/进行中返回 `(None, Some(尾注))`。
    async fn nix_env_state(
        &self,
        base: Option<&Path>,
    ) -> (Option<Arc<HashMap<String, String>>>, Option<String>) {
        let Some(cache) = &self.nix_env else {
            return (None, None);
        };
        let workspace = base
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok());
        let Some(workspace) = workspace else {
            return (None, None);
        };
        match cache.env_for(&workspace).await {
            Ok(Some(env)) => (Some(env), None),
            Ok(None) => (None, None),
            Err(reason) => (
                None,
                Some(format!("[nix] {reason}; ran with host environment")),
            ),
        }
    }

    /// `cd <uri>`（且仅这种单一命令形态）重写为底层路径；URI 解析失败
    /// 与虚拟资源（无 source_path）报错，其余命令原样返回。
    async fn rewrite_cd_uri(&self, command: &str) -> Result<String, ToolError> {
        let Some(router) = &self.vfs_router else {
            return Ok(command.to_string());
        };
        let trimmed = command.trim();
        let Some(rest) = trimmed.strip_prefix("cd ") else {
            return Ok(command.to_string());
        };
        // 支持 `cd <uri>` 与 `cd <uri> && …` 两种形态；管道/重定向/分号
        // 等其余复合形态不重写（保守）
        let (arg, tail) = match rest.find(char::is_whitespace) {
            Some(index) => (&rest[..index], rest[index..].trim_start()),
            None => (rest, ""),
        };
        if (!tail.is_empty() && !tail.starts_with("&&"))
            || arg.contains(['&', '|', ';', '>', '<', '`', '$', '\n'])
        {
            return Ok(command.to_string());
        }
        let arg = arg.trim_matches(|c| c == '\'' || c == '"');
        if !router.can_resolve(arg) {
            return Ok(command.to_string());
        }
        let path = crate::vfs_guard::vfs_source_path(router, arg, "bash")
            .await?
            .expect("can_resolve 蕴含已注册");
        let escaped = path.display().to_string().replace('\'', "'\\''");
        if tail.is_empty() {
            Ok(format!("cd '{escaped}'"))
        } else {
            Ok(format!("cd '{escaped}' {tail}"))
        }
    }
}

const LABEL: &str = "bash";

/// 配置并 spawn bash 子进程：管道输出、`kill_on_drop`、以基准目录为执行
/// 目录（`None` 时进程 cwd）；子进程自成一个进程组（pgid = pid）：
/// 超时/取消时按组强杀，命令派生的孙进程一并停止（见模块文档）。
/// `env` 为 nix 纯净环境（ADR-0041）：`env_clear` 后整体注入，bash 从
/// 该 env 的 PATH 显式解析（execvp 用的是宿主 PATH，不能依赖）。
fn spawn_bash(
    command_text: &str,
    base: Option<PathBuf>,
    env: Option<&HashMap<String, String>>,
) -> Result<tokio::process::Child, ToolError> {
    let program = env
        .and_then(|env| crate::nix_env::resolve_program("bash", env))
        .unwrap_or_else(|| "bash".to_string());
    let mut command = tokio::process::Command::new(program);
    if let Some(env) = env {
        command.env_clear().envs(env.iter());
    }
    command
        .arg("-c")
        .arg(command_text)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = base {
        command.current_dir(dir);
    }
    #[cfg(unix)]
    command.process_group(0);
    command
        .spawn()
        .map_err(|e| ToolError::new(format!("Could not spawn bash: {e}")))
}
const DESCRIPTION: &str = "Execute a bash command in the current working directory. If the workspace defines a nix environment (.nomic/flake.nix, readable and editable via the nix://shell URI), the command runs inside that pure environment; otherwise it falls back to the host environment. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds (defaults to 60); on timeout the process group is forcibly killed and the output collected so far is returned.";

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
        // 未显式给出时使用默认 60s：任何命令都必须有界，长时间运行的命令
        // 不能无限期占用工具调用（用户可显式调大）
        let timeout = params
            .timeout
            .map_or(DEFAULT_TIMEOUT, Duration::from_secs_f64);
        tracing::debug!(command = %params.command, ?timeout, "bash start");
        let started = Instant::now();

        let command = self.rewrite_cd_uri(&params.command).await?;
        let base = self.base.snapshot();
        let (env, host_note) = self.nix_env_state(base.as_deref()).await;
        let nix_pure = env.is_some();
        let mut child = spawn_bash(&command, base, env.as_deref())?;

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

        let outcome = wait_for_child(&mut child, timeout, &cancel).await;

        let exit_code = match &outcome {
            WaitOutcome::Exited(status) => status
                .as_ref()
                .ok()
                .and_then(std::process::ExitStatus::code),
            WaitOutcome::TimedOut | WaitOutcome::Aborted => {
                force_kill(&mut child).await;
                None
            }
        };
        // 等读取任务自然读到 EOF（abort 会丢弃管道中未读的数据）；有界等待：
        // 孙进程可能脱离进程组继续持有管道，宽限耗尽后放弃读取，
        // 已收集的输出不丢（强杀路径管道随组内进程退出立即 EOF，走不到这里）
        if tokio::time::timeout(DRAIN_GRACE, drain_readers(&mut readers))
            .await
            .is_err()
        {
            readers.abort_all();
            drain_readers(&mut readers).await;
        }
        progress_tx.send_modify(|_| {});
        drop(progress_tx);
        let _ = progress_task.await;

        let full_output = mem::take(
            &mut *buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let (output_text, details) = assemble_output(&full_output);
        let details = merge_nix_details(details, host_note.as_deref());
        let with_note = |text: String| append_host_note(text, host_note.as_deref());

        let append_status = |status_text: &str| {
            if output_text.is_empty() {
                with_note(status_text.to_string())
            } else {
                with_note(format!("{output_text}\n\n{status_text}"))
            }
        };
        match outcome {
            WaitOutcome::Aborted => Err(ToolError::new(append_status("Command aborted"))),
            WaitOutcome::TimedOut => Err(ToolError::new(append_status(&format!(
                "Command timed out after {} seconds",
                timeout.as_secs_f64()
            )))),
            WaitOutcome::Exited(_) => {
                tracing::debug!(
                    exit_code,
                    nix_pure,
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
                let mut result = ToolResult::text(with_note(if output_text.is_empty() {
                    "(no output)".to_string()
                } else {
                    output_text
                }));
                result.details = details;
                Ok(result)
            }
        }
    }
}

/// 排干全部读取任务（各自读到 EOF 或被 abort 后返回）。
async fn drain_readers(readers: &mut tokio::task::JoinSet<()>) {
    while readers.join_next().await.is_some() {}
}

/// 强制停止子进程：SIGKILL 整个进程组（unix，见模块文档），非 unix 退化为
/// 只杀直接子进程；随后 reap 子进程。
async fn force_kill(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // 子进程经 process_group(0) 成为进程组组长，pgid 即 pid；
            // pid 来自 OS 必然非负
            #[allow(clippy::cast_possible_wrap)]
            let pgid = nix::unistd::Pid::from_raw(pid as i32);
            // 组内进程已全部退出（ESRCH）不算错误
            let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
        }
        let _ = child.wait().await;
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
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
    timeout: Duration,
    cancel: &CancellationToken,
) -> WaitOutcome {
    tokio::select! {
        biased;
        () = cancel.cancelled() => WaitOutcome::Aborted,
        status = child.wait() => WaitOutcome::Exited(status),
        () = tokio::time::sleep(timeout) => WaitOutcome::TimedOut,
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

/// nix 回退尾注（ADR-0041）：附加到输出尾部，风格同截断 notice。
fn append_host_note(text: String, note: Option<&str>) -> String {
    match note {
        Some(note) if text.is_empty() => note.to_string(),
        Some(note) => format!("{text}\n\n{note}"),
        None => text,
    }
}

/// 把 nix 回退信息并入 details（与截断 details 共存），供上层结构化消费。
fn merge_nix_details(
    details: Option<serde_json::Value>,
    note: Option<&str>,
) -> Option<serde_json::Value> {
    let Some(note) = note else {
        return details;
    };
    let mut value = details.unwrap_or_else(|| serde_json::json!({}));
    value["nix"] = serde_json::json!({ "mode": "host", "reason": note });
    Some(value)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_update() -> ToolUpdateCallback {
        Box::new(|_| {})
    }

    async fn run(command: &str, timeout: Option<f64>) -> Result<ToolResult, ToolError> {
        BashTool::new()
            .execute(
                BashParams {
                    command: command.to_string(),
                    timeout,
                },
                CancellationToken::new(),
                noop_update(),
            )
            .await
    }

    #[tokio::test]
    async fn success_returns_output() {
        let result = run("echo hello", None).await.expect("echo 应成功");
        let [nomic_ai::UserContent::Text(text)] = &result.content[..] else {
            panic!("expected text result");
        };
        assert_eq!(text.text.trim_end(), "hello");
    }

    #[tokio::test]
    async fn non_zero_exit_is_error_with_output() {
        let error = run("echo out; exit 3", None).await.unwrap_err();
        assert!(error.to_string().contains("out"), "{error}");
        assert!(error.to_string().contains("exited with code 3"), "{error}");
    }

    #[tokio::test]
    async fn invalid_timeout_rejected() {
        for timeout in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let error = run("echo hi", Some(timeout)).await.unwrap_err();
            assert!(error.to_string().contains("Invalid timeout"), "{error}");
        }
    }

    /// 超时强杀并返回已收集的输出。
    #[tokio::test]
    async fn timeout_kills_and_returns_collected_output() {
        let started = Instant::now();
        let error = run("echo before; sleep 30", Some(0.5)).await.unwrap_err();
        let elapsed = started.elapsed();
        assert!(error.to_string().contains("before"), "{error}");
        assert!(
            error.to_string().contains("timed out after 0.5 seconds"),
            "{error}"
        );
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    }

    /// 孙进程持有输出管道时超时仍能返回：只杀 shell 会让 `sleep` 存活并
    /// 持有管道，读取任务等不到 EOF（进程组强杀回归测试）。
    #[tokio::test]
    async fn timeout_kills_process_group_holding_pipes() {
        let started = Instant::now();
        let error = run("echo first; sleep 30 & sleep 30", Some(0.5))
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(error.to_string().contains("first"), "{error}");
        assert!(error.to_string().contains("timed out"), "{error}");
        // 排干宽限（5s）也不应走到：进程组强杀后管道立即 EOF
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }

    /// 显式取消同样强杀进程组并返回已收集输出。
    #[tokio::test]
    async fn abort_kills_and_returns_collected_output() {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel_clone.cancel();
        });
        let error = BashTool::new()
            .execute(
                BashParams {
                    command: "echo partial; sleep 30".to_string(),
                    timeout: None,
                },
                cancel,
                noop_update(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("partial"), "{error}");
        assert!(error.to_string().contains("aborted"), "{error}");
    }

    /// nix 环境解析失败（坏 flake 或无 nix）时回退宿主环境：命令照常
    /// 执行，输出尾部附提示，details 记录 mode/reason（ADR-0041）。
    #[tokio::test]
    async fn nix_env_failure_falls_back_to_host_with_note() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let nomic_dir = dir.path().join(".nomic");
        std::fs::create_dir_all(&nomic_dir).expect("mkdir");
        // 语法错误的 flake：nix develop 求值立即失败（不拉取 inputs）
        std::fs::write(nomic_dir.join("flake.nix"), "{ this is not valid nix !!!")
            .expect("write flake");
        let tool = BashTool::new()
            .with_base_dir(Some(dir.path().to_path_buf()))
            .with_nix_env(crate::nix_env::NixEnvCache::new());
        let result = tool
            .execute(
                BashParams {
                    command: "echo ok".to_string(),
                    timeout: Some(30.0),
                },
                CancellationToken::new(),
                noop_update(),
            )
            .await
            .expect("fallback should run on host");
        let [nomic_ai::UserContent::Text(text)] = &result.content[..] else {
            panic!("expected text result");
        };
        assert!(text.text.contains("ok"), "{}", text.text);
        assert!(
            text.text.contains("ran with host environment"),
            "{}",
            text.text
        );
        assert_eq!(
            result.details.as_ref().expect("details")["nix"]["mode"],
            "host"
        );
    }

    /// 未注入缓存时行为与注入前完全一致（无 nix 尾注）。
    #[tokio::test]
    async fn without_nix_cache_no_note() {
        let result = run("echo plain", None).await.expect("echo");
        let [nomic_ai::UserContent::Text(text)] = &result.content[..] else {
            panic!("expected text result");
        };
        assert_eq!(text.text.trim_end(), "plain");
        assert!(result.details.is_none());
    }
}
