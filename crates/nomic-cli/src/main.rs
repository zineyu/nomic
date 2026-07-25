//! nomic：Rust 编码 agent CLI（pi-coding-agent 的 Rust 复刻，见 docs/adr/0001）。
//!
//! M1：print 模式（`-p`），流式输出到 stdout，工具执行摘要到 stderr。
//!
//! session 持久化（方案 A，事件驱动）：core 零改动，CLI 在事件流中对每条
//! `MessageEnd`（消息定稿点）调 [`SessionStore::append_message`] 落库；
//! 持久化失败仅告警不中断运行（store 非权威源）。
//!
//! resume：`--continue`/`-c` 恢复最近一次 session，`--session <ID>` 恢复指定
//! session；历史消息经 [`Agent::with_messages`] 注入，新消息续写到同一 session。

use std::io::Write as _;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use nomic_ai::{
    ApiKind, AssistantEvent, Message, Model, Provider, StopReason, StreamOptions, ThinkingLevel,
    providers::{AnthropicProvider, OpenAiCompat, OpenAiProvider},
};
use nomic_core::{Agent, AgentConfig, AgentEvent, ExecutionMode, NoopHooks};
use nomic_session::SessionStore;
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

    /// 恢复最近一次 session 继续对话
    #[arg(long = "continue", short = 'c', conflicts_with = "session")]
    continue_session: bool,

    /// 恢复指定 id 的 session 继续对话
    #[arg(long, value_name = "ID")]
    session: Option<String>,
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

    // session 初始化需在构建 agent 前完成：resume 的历史要注入 agent
    let (session, history) = match init_session(&cli).await? {
        Some(init) => {
            eprintln!(
                "\x1b[2msession {}（{} 条历史消息）\x1b[0m",
                init.id,
                init.history.len()
            );
            (Some((init.store, init.id)), init.history)
        }
        None => (None, Vec::new()),
    };

    let (mut agent, mut events) = Agent::with_messages(
        AgentConfig {
            model,
            provider,
            stream_options,
            hooks: Arc::new(NoopHooks),
            tool_execution: ExecutionMode::Parallel,
        },
        nomic_tools::default_tools(),
        build_system_prompt(cli.append_system.as_deref())?,
        history,
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

    let saw_error = drain_events(&mut events, session.as_ref()).await;

    let result = run.await.context("prompt task panicked")?;
    if let Err(error) = result {
        bail!("agent loop failed: {error}");
    }
    if let Some(error) = saw_error {
        bail!("{error}");
    }
    Ok(())
}

/// 消费 agent 事件流：流式输出到 stdout/stderr，消息定稿点落库。
///
/// 返回运行中见过的 provider 错误（编码在 assistant 消息里）。
async fn drain_events(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    session: Option<&(SessionStore, String)>,
) -> Option<String> {
    let mut saw_error: Option<String> = None;
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::MessageUpdate(AssistantEvent::TextDelta { delta, .. }) => {
                // 锁不跨 await 持有（StdoutLock 非 Send）
                print!("{delta}");
                let _ = std::io::stdout().flush();
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
                // 消息定稿点：按事件顺序追加（parent_id=None 自动链到最新 entry）
                if let Some((store, session_id)) = session {
                    if let Err(error) = store.append_message(session_id, None, &message).await {
                        eprintln!("\x1b[33m⚠ session 落库失败：{error}\x1b[0m");
                    }
                }
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
    saw_error
}

/// session 初始化结果：store、session id 与恢复的历史消息（新会话为空）。
struct SessionInit {
    store: SessionStore,
    id: String,
    history: Vec<Message>,
}

/// 初始化 session：按 `--continue`/`--session` 恢复既有会话，否则新建。
///
/// - resume 语义下打开库/加载消息失败直接报错（用户显式要求恢复）
/// - 新会话语义下降级为不持久化（打告警后返回 `None`），不阻断本次运行
async fn init_session(cli: &Cli) -> Result<Option<SessionInit>> {
    let resume = cli.continue_session || cli.session.is_some();
    let store = match SessionStore::open_default().await {
        Ok(store) => store,
        Err(error) => {
            if resume {
                return Err(error).context("打开 session 库失败，无法恢复会话");
            }
            eprintln!("\x1b[33m⚠ 打开 session 库失败，本次运行不持久化：{error}\x1b[0m");
            return Ok(None);
        }
    };

    if resume {
        let id = match &cli.session {
            Some(id) => id.clone(),
            None => store
                .list_sessions()
                .await
                .context("列出 session 失败")?
                .into_iter()
                .next()
                .map(|summary| summary.id)
                .context("没有可恢复的 session")?,
        };
        let history = store
            .load_messages(&id)
            .await
            .with_context(|| format!("加载 session {id} 失败"))?;
        return Ok(Some(SessionInit { store, id, history }));
    }

    let cwd = std::env::current_dir().context("get cwd")?;
    match store.create_session(&cwd).await {
        Ok(id) => Ok(Some(SessionInit {
            store,
            id,
            history: Vec::new(),
        })),
        Err(error) => {
            eprintln!("\x1b[33m⚠ 创建 session 失败，本次运行不持久化：{error}\x1b[0m");
            Ok(None)
        }
    }
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
