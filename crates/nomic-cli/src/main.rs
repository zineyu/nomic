//! nomic：Rust 编码 agent CLI（pi-coding-agent 的 Rust 复刻，见 docs/adr/0001）。
//!
//! 两种运行模式：
//! - print 模式（`-p/--print`）：非交互，流式输出到 stdout，管道可用
//! - 交互 TUI（缺省）：ratatui 全屏界面（见 docs/adr/0002）
//!
//! 本文件只做 CLI 解析与模式分发；共享的 provider/model/session 初始化在
//! `bootstrap`，print 模式在 `print`，交互模式在 `tui`，session 管理子命令在
//! `sessions`。

mod bootstrap;
mod clipboard;
mod config;
mod context_files;
mod images;
mod logging;
mod print;
mod sessions;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::logging::LogTarget;

/// Rust 编码 agent（pi-coding-agent 的 Rust 复刻）。
#[derive(Debug, Clone, Parser)]
#[command(name = "nomic", version, about)]
pub(crate) struct Cli {
    /// 要发送的 prompt（print 模式，非交互；缺省进入交互 TUI）
    #[arg(short, long, value_name = "TEXT")]
    pub(crate) print: Option<String>,

    /// 随 prompt 发送的图片附件（可重复传入；png/jpeg/gif/webp）
    #[arg(long, value_name = "PATH")]
    pub(crate) image: Vec<std::path::PathBuf>,

    /// provider：config.toml 的 `[providers]` 中定义的名字（anthropic、openai 可按名推断 api）；
    /// 需搭配 `--model`（无内置默认模型，缺省用数据库中保存的选择）
    #[arg(long)]
    pub(crate) provider: Option<String>,

    /// 模型 id，支持 `<provider>/<模型id>` 全形式跨 provider 指定
    /// （缺省用数据库中保存的选择；都没有时启动报错，无内置默认模型）
    #[arg(long)]
    pub(crate) model: Option<String>,

    /// API base URL（缺省按 provider；也可用 OPENAI_BASE_URL）
    #[arg(long)]
    pub(crate) base_url: Option<String>,

    /// API key（缺省读 ANTHROPIC_API_KEY / OPENAI_API_KEY）
    #[arg(long)]
    pub(crate) api_key: Option<String>,

    /// 推理级别：minimal/low/medium/high（缺省不开启）
    #[arg(long, value_parser = ["minimal", "low", "medium", "high"])]
    pub(crate) reasoning: Option<String>,

    /// 采样温度
    #[arg(long)]
    pub(crate) temperature: Option<f64>,

    /// 最大输出 token 数
    #[arg(long)]
    pub(crate) max_tokens: Option<u64>,

    /// 追加到系统提示词末尾的文本
    #[arg(long)]
    pub(crate) append_system: Option<String>,

    /// 显式激活一个 skill（可重复传入）
    #[arg(long, value_name = "NAME")]
    pub(crate) skill: Vec<String>,

    /// 额外的 prompt template 文件或目录（可重复传入；优先级高于项目/用户目录）
    #[arg(long, value_name = "PATH")]
    pub(crate) prompt_template: Vec<std::path::PathBuf>,

    /// 禁用 prompt template 目录发现（显式指定的路径仍生效）
    #[arg(long)]
    pub(crate) no_prompt_templates: bool,

    /// 恢复最近一次 session 继续对话
    #[arg(long = "continue", short = 'c', conflicts_with = "session")]
    pub(crate) continue_session: bool,

    /// 恢复指定 id 的 session 继续对话（内部/调试用：id 不对外展示，
    /// 一般用 `nomic resume` 交互选择或 `--continue` 恢复最近的 session）
    #[arg(long, value_name = "ID", hide = true)]
    pub(crate) session: Option<String>,

    /// 日志输出目标：file 默认写入 XDG state 目录并按天滚动；
    /// terminal 输出到 stderr（TUI 模式下会干扰界面）；off 关闭
    #[arg(long, value_enum, default_value = "file")]
    pub(crate) log: LogTarget,

    /// 日志过滤规则（tracing 指令语法，如 debug、nomic=trace；高于 RUST_LOG）
    #[arg(long, value_name = "FILTER")]
    pub(crate) log_level: Option<String>,

    /// 子命令（session 管理等）
    #[command(subcommand)]
    pub(crate) command: Option<Commands>,
}

/// 顶层子命令。
#[derive(Debug, Clone, Subcommand)]
pub(crate) enum Commands {
    /// 交互选择并恢复历史 session
    Resume,
    /// 管理历史 session
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
    },
}

/// `nomic sessions` 子命令。
#[derive(Debug, Clone, Subcommand)]
pub(crate) enum SessionsCommand {
    /// 列出全部 session（标题、最后更新时间、消息数、目录）
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // guard 必须活到进程退出，否则非阻塞 writer 尾部缓冲丢失
    let _log_guard = logging::init(cli.log, cli.log_level.as_deref())?;
    tracing::debug!(
        version = env!("CARGO_PKG_VERSION"),
        args = ?std::env::args().collect::<Vec<_>>(),
        "nomic 启动"
    );
    match &cli.command {
        Some(Commands::Resume) => sessions::resume(&cli).await,
        Some(Commands::Sessions {
            command: SessionsCommand::List,
        }) => sessions::list().await,
        None => dispatch(&cli).await,
    }
}

/// 无子命令时的常规对话分发：print 模式或交互 TUI。
// future 非 Send 的原因与安全性见 tui/mod.rs 的模块级说明（同上）
#[allow(clippy::future_not_send)]
pub(crate) async fn dispatch(cli: &Cli) -> Result<()> {
    if let Some(prompt) = &cli.print {
        print::run(cli, prompt).await
    } else {
        tui::run(cli).await
    }
}
