//! print 模式（`-p`）：非交互，流式输出到 stdout，工具执行摘要到 stderr，
//! 退出码反映成功/失败，管道可用。
//!
//! session 持久化（事件驱动）：core 零改动，在事件流中对每条 `MessageEnd`
//! （消息定稿点）调 `SessionStore::append_message` 落库；持久化失败仅告警
//! 不中断运行（store 非权威源）。

use std::io::Write as _;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use nomic_ai::{AssistantEvent, Message, StopReason};
use nomic_core::{Agent, AgentConfig, AgentEvent, ExecutionMode, NoopHooks};
use nomic_session::SessionStore;
use tokio_util::sync::CancellationToken;

use crate::{Cli, bootstrap};

/// 运行 print 模式。
pub async fn run(cli: &Cli, prompt: &str) -> Result<()> {
    let boot = bootstrap::bootstrap(cli).await?;
    if let Some((_, id)) = &boot.session {
        eprintln!(
            "\x1b[2msession {}（{} 条历史消息）\x1b[0m",
            id,
            boot.history.len()
        );
    }

    let (mut agent, mut events) = Agent::with_messages(
        AgentConfig {
            model: boot.model,
            provider: boot.provider,
            stream_options: boot.stream_options,
            hooks: Arc::new(NoopHooks),
            tool_execution: ExecutionMode::Parallel,
        },
        nomic_tools::default_tools(),
        boot.system_prompt,
        boot.history,
    );

    let cancel = CancellationToken::new();
    let cancel_on_sigint = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel_on_sigint.cancel();
        }
    });

    let cancel_for_prompt = cancel.clone();
    let prompt = prompt.to_string();
    let run = tokio::spawn(async move { agent.prompt(&prompt, cancel_for_prompt).await });

    let saw_error = drain_events(&mut events, boot.session.as_ref()).await;

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

/// 工具参数的简短摘要（stderr / TUI 展示）。
pub fn brief_args(args: &serde_json::Value) -> String {
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
