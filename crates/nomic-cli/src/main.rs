//! nomic：Rust 编码 agent CLI（pi-coding-agent 的 Rust 复刻，见 docs/adr/0001）。
//!
//! M1：print 模式（`-p`），流式输出到 stdout，工具执行摘要到 stderr。

use std::io::Write as _;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use nomic_ai::{
    ApiKind, AssistantEvent, Message, Model, Provider, StopReason, StreamOptions, ThinkingLevel,
    providers::{AnthropicProvider, OpenAiCompat, OpenAiProvider},
};
use nomic_core::{Agent, AgentConfig, AgentEvent, ExecutionMode, NoopHooks};
use tokio_util::sync::CancellationToken;

/// Rust 编码 agent（pi-coding-agent 的 Rust 复刻）。
#[derive(Debug, Parser)]
#[command(name = "nomic", version, about)]
struct Cli {
    /// 要发送的 prompt（print 模式，非交互）
    #[arg(short, long, value_name = "TEXT")]
    print: Option<String>,

    /// provider：anthropic 或 openai（兼容端点）
    #[arg(long, value_parser = ["anthropic", "openai"])]
    provider: Option<String>,

    /// 模型 id（缺省按 provider 选择默认模型）
    #[arg(long)]
    model: Option<String>,

    /// API base URL（缺省按 provider；也可用 OPENAI_BASE_URL）
    #[arg(long)]
    base_url: Option<String>,

    /// API key（缺省读 ANTHROPIC_API_KEY / OPENAI_API_KEY）
    #[arg(long)]
    api_key: Option<String>,

    /// 推理级别：minimal/low/medium/high（缺省不开启）
    #[arg(long, value_parser = ["minimal", "low", "medium", "high"])]
    reasoning: Option<String>,

    /// 采样温度
    #[arg(long)]
    temperature: Option<f64>,

    /// 最大输出 token 数
    #[arg(long)]
    max_tokens: Option<u64>,

    /// 追加到系统提示词末尾的文本
    #[arg(long)]
    append_system: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let Some(prompt) = cli.print.clone() else {
        bail!("M1 仅支持 print 模式：nomic -p \"your prompt\"");
    };

    let provider_kind = cli.provider.clone().unwrap_or_else(|| {
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            "anthropic".to_string()
        } else {
            "openai".to_string()
        }
    });
    let model = resolve_model(&provider_kind, &cli);
    let provider: Arc<dyn Provider> = match model.api {
        ApiKind::AnthropicMessages => Arc::new(AnthropicProvider::new(cli.api_key.clone())),
        ApiKind::OpenAiCompletions => Arc::new(OpenAiProvider::new(
            cli.api_key.clone(),
            OpenAiCompat::default(),
        )),
    };

    let stream_options = StreamOptions {
        temperature: cli.temperature,
        max_tokens: cli.max_tokens,
        reasoning: cli.reasoning.as_deref().map(parse_reasoning),
        api_key: cli.api_key.clone(),
        headers: Vec::new(),
        timeout_ms: None,
    };

    let (mut agent, mut events) = Agent::new(
        AgentConfig {
            model,
            provider,
            stream_options,
            hooks: Arc::new(NoopHooks),
            tool_execution: ExecutionMode::Parallel,
        },
        nomic_tools::default_tools(),
        build_system_prompt(cli.append_system.as_deref())?,
    );

    let cancel = CancellationToken::new();
    let cancel_on_sigint = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel_on_sigint.cancel();
        }
    });

    let cancel_for_prompt = cancel.clone();
    let run = tokio::spawn(async move { agent.prompt(&prompt, cancel_for_prompt).await });

    let mut saw_error: Option<String> = None;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::MessageUpdate(AssistantEvent::TextDelta { delta, .. }) => {
                print!("{delta}");
                let _ = out.flush();
            }
            AgentEvent::MessageUpdate(AssistantEvent::ThinkingDelta { delta, .. }) => {
                eprint!("\x1b[2m{delta}\x1b[0m");
            }
            AgentEvent::ToolExecutionStart {
                tool_name, args, ..
            } => {
                eprintln!("\n\x1b[36m▶ {tool_name}\x1b[0m {}", brief_args(&args));
            }
            AgentEvent::ToolExecutionEnd {
                tool_name,
                is_error,
                ..
            } => {
                let mark = if is_error {
                    "\x1b[31m✗"
                } else {
                    "\x1b[32m✓"
                };
                eprintln!("{mark} {tool_name}\x1b[0m");
            }
            AgentEvent::MessageEnd(message) => {
                if let Message::Assistant(assistant) = *message {
                    if matches!(
                        assistant.stop_reason,
                        StopReason::Error | StopReason::Aborted
                    ) {
                        saw_error.clone_from(&assistant.error_message);
                    }
                    if !assistant.content.is_empty() {
                        println!();
                    }
                }
            }
            _ => {}
        }
    }

    let result = run.await.context("prompt task panicked")?;
    if let Err(error) = result {
        bail!("agent loop failed: {error}");
    }
    if let Some(error) = saw_error {
        bail!("{error}");
    }
    Ok(())
}

/// 解析模型：内置预设 + provider 默认值兜底。
fn resolve_model(provider_kind: &str, cli: &Cli) -> Model {
    let (api, default_model, default_base_url, reasoning, context_window, max_tokens, costs) =
        match provider_kind {
            "anthropic" => (
                ApiKind::AnthropicMessages,
                "claude-sonnet-4-5",
                "https://api.anthropic.com",
                true,
                200_000,
                64_000,
                (3.0, 15.0, 0.3, 3.75),
            ),
            _ => (
                ApiKind::OpenAiCompletions,
                "gpt-5.2",
                "https://api.openai.com/v1",
                true,
                400_000,
                128_000,
                (0.0, 0.0, 0.0, 0.0),
            ),
        };
    let base_url = cli
        .base_url
        .clone()
        .or_else(|| {
            std::env::var("OPENAI_BASE_URL")
                .ok()
                .filter(|_| api == ApiKind::OpenAiCompletions)
        })
        .unwrap_or_else(|| default_base_url.to_string());
    let id = cli
        .model
        .clone()
        .unwrap_or_else(|| default_model.to_string());
    Model {
        name: id.clone(),
        id,
        api,
        provider: provider_kind.to_string(),
        base_url,
        reasoning,
        context_window,
        max_tokens,
        cost_input: costs.0,
        cost_output: costs.1,
        cost_cache_read: costs.2,
        cost_cache_write: costs.3,
    }
}

fn parse_reasoning(level: &str) -> ThinkingLevel {
    match level {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        _ => ThinkingLevel::High,
    }
}

/// 系统提示词（对齐 pi 的结构与措辞）。
fn build_system_prompt(append: Option<&str>) -> Result<String> {
    let cwd = std::env::current_dir()
        .context("get cwd")?
        .display()
        .to_string();
    let mut prompt = "You are an expert coding assistant operating inside nomic, a coding agent harness. \
         You help users by reading files, executing commands, editing code, and writing new files.\n\n\
         Available tools:\n\
         - read: Read file contents\n\
         - bash: Execute bash commands (ls, rg, find, etc.)\n\
         - edit: Make precise file edits with exact text replacement\n\
         - write: Create or overwrite files\n\n\
         Guidelines:\n\
         - Use bash for file operations like ls, rg, find\n\
         - Use read to examine files instead of cat or sed\n\
         - Be concise in your responses\n\
         - Show file paths clearly when working with files"
        .to_string();
    if let Some(extra) = append {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
    {
        use std::fmt::Write as _;
        let _ = write!(prompt, "\n\nCurrent working directory: {cwd}");
    }
    Ok(prompt)
}

/// 工具参数的简短摘要（stderr 展示）。
fn brief_args(args: &serde_json::Value) -> String {
    const MAX: usize = 120;
    let text = args.to_string();
    if text.len() <= MAX {
        return text;
    }
    let mut index = MAX;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    format!("{}…", &text[..index])
}
