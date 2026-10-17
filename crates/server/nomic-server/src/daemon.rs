//! server 生命周期：自动拉起（detached 常驻）与停止（#39）。
//!
//! 首个客户端发现 server 未运行时，经 [`ensure_server`] detached 拉起
//! daemon。拉起分两层：中间进程 detached 拉起 daemon 本体（孙进程）
//! 后立即退出——本体脱离调用方进程组、stdio 归 null，由 init 收养，
//! 客户端退出不影响 daemon，也不会留下僵尸进程。`nomic server stop`
//! 经 pid 文件向 daemon 本体发 SIGTERM 并等待其退出（本体由 init
//! 收养，退出即被回收，信号 0 探测可靠）。

use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use nix::sys::signal::{Signal, kill};
use tokio::net::UnixStream;

use crate::client::ConnectError;

/// 默认数据目录：`~/.local/share/nomic`。
pub fn default_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME 环境变量未设置");
    PathBuf::from(home).join(".local/share/nomic")
}

/// 默认 socket 路径：数据目录下的 `nomic.sock`。
pub fn default_socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("nomic.sock")
}

/// pid 文件路径。
pub fn pid_path(data_dir: &Path) -> PathBuf {
    data_dir.join("nomic.pid")
}

/// daemon 拉起 socket 的就绪超时。
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// stop 等待 daemon 退出的超时。
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// 轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// pid 文件由 daemon 持有排它锁直到进程退出（内核在进程死亡时
/// 自动释放）：stop 据此区分「活 daemon」与「陈旧 pid」，
/// pid 被复用时也不会误发信号。
#[derive(Debug)]
pub struct PidLock {
    /// 排它锁随本结构体存活；drop 即解锁。
    _lock: nix::fcntl::Flock<std::fs::File>,
}

/// 写入 pid 并持有排它锁；锁随返回值存活。
pub fn hold_pid(data_dir: &Path) -> std::io::Result<PidLock> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(pid_path(data_dir))?;
    let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock).map_err(
        |(_file, error)| {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("pid 文件已被其他进程锁定: {error}"),
            )
        },
    )?;
    lock.write_all_at(std::process::id().to_string().as_bytes(), 0)?;
    Ok(PidLock { _lock: lock })
}

/// pid 文件是否被活 daemon 持有（flock 非阻塞探测）。
fn pid_held(data_dir: &Path) -> std::io::Result<bool> {
    let path = pid_path(data_dir);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
        // 探测成功即无人持有；drop 立即释放锁
        Ok(lock) => {
            drop(lock);
            Ok(false)
        }
        Err((_file, nix::errno::Errno::EWOULDBLOCK)) => Ok(true),
        Err((_file, error)) => Err(std::io::Error::from(error)),
    }
}

/// 读取 pid 文件；文件缺失返回 `Ok(None)`。
pub async fn read_pid(data_dir: &Path) -> std::io::Result<Option<i32>> {
    match tokio::fs::read_to_string(pid_path(data_dir)).await {
        Ok(text) => text.trim().parse::<i32>().map(Some).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("pid 文件损坏: {error}"),
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 确保 server 在运行：socket 可连接则直接返回；否则 detached 拉起
/// daemon 并等待 socket 就绪（超时或拉起失败为显式失败）。
///
/// 入口进程是双层拉起中的中间进程：它 detached 拉起 daemon 本体后
/// 立即退出，本函数回收中间进程并等待 socket 就绪。
pub async fn ensure_server(
    socket_path: &Path,
    data_dir: &Path,
    daemon_exe: &Path,
) -> Result<(), ConnectError> {
    if UnixStream::connect(socket_path).await.is_ok() {
        return Ok(());
    }
    let mut child = tokio::process::Command::new(daemon_exe)
        .arg("server")
        .arg("--socket")
        .arg(socket_path)
        .arg("--data-dir")
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // 脱离调用方进程组：终端信号（Ctrl-C 等）不传给 daemon
        .process_group(0)
        .spawn()?;

    // 中间进程拉起本体后立即退出；回收它避免僵尸
    let status = child.wait().await?;
    if !status.success() {
        return Err(
            std::io::Error::other(format!("daemon 拉起失败（中间进程 status {status}）")).into(),
        );
    }

    // 等待 socket 就绪：daemon 本体（孤儿孙进程）绑定成功即视为
    // 拉起完成
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        if UnixStream::connect(socket_path).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::other("等待 daemon 拉起超时").into());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// 停止失败类别。
#[derive(Debug, thiserror::Error)]
pub enum StopError {
    /// server 未运行（无 pid 且 socket 不可连接）。
    #[error("server 未运行")]
    NotRunning,
    /// pid 文件损坏。
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// SIGTERM 后等待退出超时。
    #[error("server 在 {0:?} 内未退出")]
    Timeout(Duration),
}

/// 停止 server：向 pid 对应进程发 SIGTERM 并等待退出，随后清理
/// socket 与 pid 文件（daemon 自身也做清理，双保险容忍其未及执行）。
pub async fn stop_server(socket_path: &Path, data_dir: &Path) -> Result<(), StopError> {
    let pid = match read_pid(data_dir).await? {
        Some(pid) => pid,
        None => wait_pid_file(data_dir).await?,
    };
    if !pid_held(data_dir)? {
        // 陈旧现场（无活 daemon 持有 pid 锁，pid 可能已被复用）：
        // 绝不向该 pid 发信号，清理残留后显式报告未运行
        let _ = tokio::fs::remove_file(pid_path(data_dir)).await;
        let _ = tokio::fs::remove_file(socket_path).await;
        return Err(StopError::NotRunning);
    }
    signal_sigterm(pid)?;
    wait_exit(pid).await?;
    let _ = tokio::fs::remove_file(pid_path(data_dir)).await;
    let _ = tokio::fs::remove_file(socket_path).await;
    Ok(())
}

/// pid 文件尚不存在：daemon 可能刚被拉起尚未写入，先等 socket、
/// 再短暂轮询 pid 文件；仍无则判定未运行。
async fn wait_pid_file(data_dir: &Path) -> Result<i32, StopError> {
    let deadline = tokio::time::Instant::now() + START_TIMEOUT.min(STOP_TIMEOUT);
    loop {
        match read_pid(data_dir).await? {
            Some(pid) => return Ok(pid),
            None if tokio::time::Instant::now() >= deadline => {
                return Err(StopError::NotRunning);
            }
            None => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }
}

/// 进程是否存活（信号 0 探测）。
fn alive(pid: i32) -> bool {
    kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

/// 发送 SIGTERM。
fn signal_sigterm(pid: i32) -> Result<(), StopError> {
    kill(nix::unistd::Pid::from_raw(pid), Some(Signal::SIGTERM)).map_err(std::io::Error::from)?;
    Ok(())
}

/// 轮询等待进程退出。
async fn wait_exit(pid: i32) -> Result<(), StopError> {
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    loop {
        if !alive(pid) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(StopError::Timeout(STOP_TIMEOUT));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
