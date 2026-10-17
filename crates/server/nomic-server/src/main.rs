//! nomic CLI：`nomic server` 常驻 daemon，`nomic server stop` 停止。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use nomic_server::daemon::{self, UnconfiguredLlm};
use nomic_server::environment::LocalEnvironment;
use nomic_server::ipc::{self, IpcServer};
use nomic_server::storage::SQLiteStorage;

#[derive(Debug, Parser)]
#[command(name = "nomic", about = "nomic 常驻 server 与终端客户端")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// server 生命周期管理。
    Server {
        #[command(subcommand)]
        action: Option<ServerAction>,
        /// socket 路径（默认 <数据目录>/nomic.sock）。
        #[arg(long, global = true)]
        socket: Option<PathBuf>,
        /// 数据目录（默认 ~/.local/share/nomic）。
        #[arg(long, global = true)]
        data_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum ServerAction {
    /// 停止运行中的 server。
    Stop,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Command::Server {
        action,
        socket,
        data_dir,
    } = cli.command;
    let data_dir = data_dir.unwrap_or_else(daemon::default_data_dir);
    let socket = socket.unwrap_or_else(|| daemon::default_socket_path(&data_dir));
    let result = match action {
        None => serve(&data_dir, &socket),
        Some(ServerAction::Stop) => stop(&data_dir, &socket),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// daemon 常驻的环境变量标记：本体（孙进程）启动时设置。
const DAEMON_CHILD_ENV: &str = "NOMIC_DAEMON_CHILD";

/// server 入口：未带本体标记时是双层拉起的中间进程——detached 拉起
/// 本体（孙进程）并等待其 socket 就绪后退出；本体由 init 收养，
/// 退出即被回收（无僵尸），客户端断开不影响 daemon。
fn serve(data_dir: &std::path::Path, socket_path: &std::path::Path) -> Result<(), String> {
    if std::env::var_os(DAEMON_CHILD_ENV).is_none() {
        return supervise(data_dir, socket_path);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("启动运行时失败: {error}"))?;
    runtime.block_on(serve_async(data_dir, socket_path))
}

/// 中间进程：detached 拉起本体，轮询 socket 就绪后退出。
fn supervise(data_dir: &std::path::Path, socket_path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|error| format!("定位自身失败: {error}"))?;
    std::process::Command::new(exe)
        .arg("server")
        .arg("--socket")
        .arg(socket_path)
        .arg("--data-dir")
        .arg(data_dir)
        .env(DAEMON_CHILD_ENV, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("拉起 daemon 本体失败: {error}"))?;
    // 轮询 socket 就绪；本体启动失败时由调用方（ensure_server）的
    // 就绪超时兜底
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if std::os::unix::net::UnixStream::connect(socket_path).is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("daemon 本体拉起超时".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// daemon 本体：写 pid、绑定 socket、服务到 SIGTERM。
async fn serve_async(
    data_dir: &std::path::Path,
    socket_path: &std::path::Path,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::create_dir_all(data_dir)
        .await
        .map_err(|error| format!("创建数据目录失败: {error}"))?;
    tokio::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|error| format!("设置数据目录权限失败: {error}"))?;

    // 信号处理先于 pid 写入安装：早退也能清理现场
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| format!("安装 SIGTERM 处理失败: {error}"))?;

    let storage = SQLiteStorage::open(data_dir.join("nomic.db"))
        .map_err(|error| format!("打开存储失败: {error}"))?;
    let server = IpcServer::new(
        Arc::new(storage),
        Arc::new(UnconfiguredLlm),
        Arc::new(LocalEnvironment::new()),
    );
    // 先绑定再写 pid：绑定失败的第二实例不得覆盖活 daemon 的 pid
    let listener = ipc::bind_listener(socket_path)
        .await
        .map_err(|error| format!("绑定 socket 失败: {error}"))?;
    let pid_lock =
        daemon::hold_pid(data_dir).map_err(|error| format!("写入并锁定 pid 文件失败: {error}"))?;

    let result = tokio::select! {
        result = server.serve(listener) => {
            result.map_err(|error| format!("IPC 服务失败: {error}"))
        }
        _ = terminate.recv() => Ok(()),
    };
    // 退出路径（SIGTERM 或服务故障）都必须清理现场
    let _ = tokio::fs::remove_file(socket_path).await;
    let _ = tokio::fs::remove_file(daemon::pid_path(data_dir)).await;
    drop(pid_lock);
    result
}

fn stop(data_dir: &std::path::Path, socket_path: &std::path::Path) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("启动运行时失败: {error}"))?;
    runtime
        .block_on(daemon::stop_server(socket_path, data_dir))
        .map_err(|error| error.to_string())
}
