//! 日志系统：tracing subscriber 装配与 XDG 日志目录解析。
//!
//! 输出目标由 `--log` 选择：
//! - `file`（默认）：写入 XDG state 目录（`$XDG_STATE_HOME/nomic/logs`，
//!   fallback `~/.local/state/nomic/logs`），按天滚动（`nomic.log.YYYY-MM-DD`），
//!   经 tracing-appender 的非阻塞 writer 落盘；
//! - `terminal`：输出到 stderr（print 模式调试用；TUI 模式下会干扰界面）；
//! - `off`：关闭日志。
//!
//! 过滤规则优先级：`--log-level` > `RUST_LOG` 环境变量 > [`DEFAULT_FILTER`]。
//! 日志永不写 stdout，print 模式的管道输出不受污染。

use std::ffi::OsStr;
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

/// 默认日志目录：`$XDG_STATE_HOME/nomic/logs`，fallback `~/.local/state/nomic/logs`。
///
/// 手写解析 XDG，不引入 `dirs` 依赖（与 `config` / `nomic-session` 一致）；
/// 无 `HOME` 时返回 io 错误。
fn default_log_dir() -> Result<PathBuf> {
    log_dir_from(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
    .ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve default log dir: neither XDG_STATE_HOME nor HOME is set",
        )
        .into()
    })
}

/// 日志目录解析的纯函数内核（可测试）。
fn log_dir_from(xdg_state: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(xdg) = xdg_state
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("nomic").join("logs"));
    }
    home.map(|home| PathBuf::from(home).join(".local/state/nomic/logs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_state_home_takes_precedence() {
        let dir = log_dir_from(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/xdg/nomic/logs")));
    }

    #[test]
    fn empty_xdg_state_home_falls_back_to_home() {
        let dir = log_dir_from(Some(OsStr::new("")), Some(OsStr::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.local/state/nomic/logs")));
    }

    #[test]
    fn home_fallback_without_xdg() {
        let dir = log_dir_from(None, Some(OsStr::new("/home/u")));
        assert_eq!(dir, Some(PathBuf::from("/home/u/.local/state/nomic/logs")));
    }

    #[test]
    fn unresolvable_without_xdg_or_home() {
        assert_eq!(log_dir_from(None, None), None);
    }

    #[test]
    fn flag_level_wins_over_default() {
        // EnvFilter 无 PartialEq / 公开访问器，以 Debug 文本断言指令被采纳
        let filter = resolve_filter(Some("nomic=trace,warn"));
        let text = format!("{filter:?}");
        assert!(text.contains("Some(\"nomic\")"), "{text}");
        assert!(text.contains("TRACE"), "{text}");
    }
}
