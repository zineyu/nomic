//! nomic：Rust 编码 agent CLI（pi-coding-agent 的 Rust 复刻，见 docs/adr/0001）。
//!
//! 两种运行模式：
//! - print 模式（`-p/--print`）：非交互，流式输出到 stdout，管道可用
//! - 交互 TUI（缺省）：ratatui 全屏界面（见 docs/adr/0002）
//!
//! 本文件只做 CLI 解析与模式分发；共享的 provider/model/session 初始化在
//! `bootstrap`，print 模式在 `print`，交互模式在 `tui`。

mod bootstrap;
mod config;
mod print;
mod tui;

use anyhow::Result;
use clap::Parser;

/// Rust 编码 agent（pi-coding-agent 的 Rust 复刻）。
#[derive(Debug, Parser)]
#[command(name = "nomic", version, about)]
pub(crate) struct Cli {
    /// 要发送的 prompt（print 模式，非交互；缺省进入交互 TUI）
    #[arg(short, long, value_name = "TEXT")]
    pub(crate) print: Option<String>,

    /// provider：anthropic 或 openai（兼容端点）
    #[arg(long, value_parser = ["anthropic", "openai"])]
    pub(crate) provider: Option<String>,

    /// 模型 id（缺省按 provider 选择默认模型）
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

    /// 恢复最近一次 session 继续对话
    #[arg(long = "continue", short = 'c', conflicts_with = "session")]
    pub(crate) continue_session: bool,

    /// 恢复指定 id 的 session 继续对话
    #[arg(long, value_name = "ID")]
    pub(crate) session: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(prompt) = &cli.print {
        print::run(&cli, prompt).await
    } else {
        tui::run(&cli).await
    }
}
