//! 日志系统：tracing subscriber 装配与平台标准日志目录解析。
//!
//! 输出目标由 `--log` 选择：
//! - `file`（默认）：写入平台标准 state 目录下的 `nomic/logs`（由 `dirs` 解析：
//!   Linux 为 `$XDG_STATE_HOME` 或 `~/.local/state`；无 state 目录定义的平台回退
//!   data 目录），按天滚动（`nomic.log.YYYY-MM-DD`），
//!   经 tracing-appender 的非阻塞 writer 落盘；
//! - `terminal`：输出到 stderr（print 模式调试用；TUI 模式下会干扰界面）；
//! - `off`：关闭日志。
//!
//! 过滤规则优先级：`--log-level` > `RUST_LOG` 环境变量 > [`DEFAULT_FILTER`]。
//! 日志永不写 stdout，print 模式的管道输出不受污染。

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::ValueEnum;

/// 日志输出目标（`--log` 的取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum LogTarget {
    /// 写入 XDG state 目录下的滚动日志文件（默认）
    #[default]
    File,
    /// 输出到终端 stderr（建议配合 -p 使用；TUI 模式下会干扰界面）
    Terminal,
    /// 关闭日志
    Off,
}

/// 内置默认过滤规则：自身 crate 开到 debug，第三方保持 info。
const DEFAULT_FILTER: &str = "nomic=debug,info";

/// 日志 guard：持有期间非阻塞 writer 的后台线程持续刷写，
/// 必须在进程退出前保持存活（drop 时刷完剩余缓冲）。
#[derive(Debug)]
pub struct LogGuard {
    _file: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// 按 CLI 参数初始化全局日志；`level` 为 `--log-level` 的原始取值。
///
/// 文件日志目录创建失败时硬报错：静默降级为无日志会让
/// 「为什么没有日志文件」难以排查。
pub fn init(target: LogTarget, level: Option<&str>) -> Result<LogGuard> {
    let filter = resolve_filter(level);
    match target {
        LogTarget::File => {
            let dir = default_log_dir()?;
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("创建日志目录失败：{}", dir.display()))?;
            let appender = tracing_appender::rolling::daily(&dir, "nomic.log");
            let (writer, guard) = tracing_appender::non_blocking(appender);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(writer)
                .init();
            tracing::debug!(dir = %dir.display(), "日志写入文件");
            Ok(LogGuard { _file: Some(guard) })
        }
        LogTarget::Terminal => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .init();
            Ok(LogGuard { _file: None })
        }
        LogTarget::Off => Ok(LogGuard { _file: None }),
    }
}

/// 过滤规则：`--log-level` > `RUST_LOG` > [`DEFAULT_FILTER`]。
fn resolve_filter(level: Option<&str>) -> tracing_subscriber::EnvFilter {
    level.map_or_else(
        || {
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_FILTER))
        },
        tracing_subscriber::EnvFilter::new,
    )
}

/// 默认日志目录：平台标准 state 目录下的 `nomic/logs`（由 `dirs` 解析）；
/// `state_dir` 仅 Linux 有定义，其余平台回退 data 目录。
///
/// 无法解析标准目录时返回 io 错误。
fn default_log_dir() -> Result<PathBuf> {
    let dir = dirs::state_dir().or_else(dirs::data_dir).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve default log dir: no platform state/data directory",
        )
    })?;
    Ok(dir.join("nomic").join("logs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_level_wins_over_default() {
        // EnvFilter 无 PartialEq / 公开访问器，以 Debug 文本断言指令被采纳
        let filter = resolve_filter(Some("nomic=trace,warn"));
        let text = format!("{filter:?}");
        assert!(text.contains("Some(\"nomic\")"), "{text}");
        assert!(text.contains("TRACE"), "{text}");
    }
}
