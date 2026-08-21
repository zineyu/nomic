//! print 模式（`-p`）：非交互，流式输出到 stdout，工具执行摘要到 stderr，
//! 退出码反映成功/失败，管道可用。
//!
//! session 持久化：core 零改动，事件流经 [`SessionRecorder`] 落库（定稿点
//! 与父指针推进收在 recorder 内）；持久化失败仅告警不中断运行（store
//! 非权威源）。

use std::io::Write as _;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use nomic_ai::{AssistantEvent, Message, StopReason};
use nomic_core::{Agent, AgentEvent, ToolError};
use nomic_session::SessionRecorder;
use nomic_tools::{AskUserAnswer, AskUserQuestion, CUSTOM_OPTION, QuestionKind, QuestionSink};
use tokio_util::sync::CancellationToken;

use crate::{Cli, agent_recipe, bootstrap};

/// 运行 print 模式。
pub async fn run(cli: &Cli, prompt: &str) -> Result<()> {
    let boot = bootstrap::bootstrap(cli, bootstrap::SessionPolicy::Init).await?;
    // `/name args` 视为 prompt template 调用：展开后发送；未知名称硬报错
    let prompt = match nomic_prompts::expand_invocation(&boot.prompt_templates, prompt) {
        Ok(expanded) => expanded.unwrap_or_else(|| prompt.to_string()),
        Err(error) => return Err(error).context("展开 prompt template 失败"),
    };
    let images = load_images(&cli.image)?;
    if boot.session.is_some() {
        // session id 是内部标识，展示会话标题（首条用户消息摘要）
        let label = nomic_session::session_title(&boot.history)
            .map_or_else(|| "新会话".to_string(), |title| format!("「{title}」"));
        eprintln!("\x1b[2m{label}（{} 条历史消息）\x1b[0m", boot.history.len());
    }

    // 工具配方（组装收在 agent_recipe 模块）：print 的差异点——主/子
    // agent 各自独立的 todo 清单（非交互，无进度观察方）、提问走 stdin、
    // 无 turn 注入点；工具相对路径以 session 的 workspace 为基准（严格归属）
    let recipe = agent_recipe::assemble(agent_recipe::RecipeOpts {
        base: nomic_tools::BaseDir::new(Some(boot.workspace.clone())),
        skill_resolver: boot.skill_resolver.clone(),
        question_sink: std::sync::Arc::new(StdinQuestionSink),
        todo: agent_recipe::TodoPolicy::Isolated,
        provider: boot.provider.clone(),
        available_models: boot.available_models,
        turn_injection: None,
    });
    let (agent, mut events) = recipe
        .apply(
            Agent::builder()
                .model(boot.model.clone())
                .provider(boot.provider.clone())
                .system_prompt(boot.system_prompt),
        )
        .messages(boot.history)
        .stream_options(boot.stream_options)
        .compaction(boot.compaction)
        .build();
    // actor 模型（ADR-0022）：agent 本体移入专属任务，经 handle 驱动
    let (handle, _actor_task) = agent.spawn();

    let cancel = CancellationToken::new();
    let cancel_on_sigint = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel_on_sigint.cancel();
        }
    });

    let cancel_for_prompt = cancel.clone();
    let run = tokio::spawn(async move {
        handle
            .prompt_with_images(&prompt, &images, cancel_for_prompt)
            .await
    });

    let mut recorder = boot
        .session
        .map(|(store, id)| SessionRecorder::new(store, id));
    let saw_error = drain_events(&mut events, recorder.as_mut()).await;

    let result = run.await.context("prompt task panicked")?;
    if let Err(error) = result {
        bail!("agent loop failed: {error}");
    }
    if let Some(error) = saw_error {
        bail!("{error}");
    }
    Ok(())
}

/// [`QuestionSink`] 的 print 模式实现：问题渲染到 stderr（stdout 保持
/// 流式输出纯净，管道可用），回答从 stdin 读取。
///
/// 单选/多选按编号选择；自定义选项（末位）选中后二次输入文本；
/// 直接输入非编号文本也视为自定义答案。stdin 关闭（EOF）时报错。
struct StdinQuestionSink;

#[async_trait]
impl QuestionSink for StdinQuestionSink {
    async fn ask(
        &self,
        question: AskUserQuestion,
        cancel: CancellationToken,
    ) -> Result<AskUserAnswer, ToolError> {
        tokio::select! {
            () = cancel.cancelled() => Err(ToolError::new("question cancelled (run aborted)")),
            answer = tokio::task::spawn_blocking(move || prompt_stdin(&question)) => {
                match answer {
                    Ok(answer) => answer,
                    Err(join) => Err(ToolError::new(format!("reading stdin failed: {join}"))),
                }
            }
        }
    }
}

/// 交互式提问的阻塞实现（spawn_blocking 内运行）：打印问题与编号选项，
/// 读取一行回答并解析为 [`AskUserAnswer`]。
fn prompt_stdin(question: &AskUserQuestion) -> Result<AskUserAnswer, ToolError> {
    use std::io::BufRead as _;

    let kind_label = match question.kind {
        QuestionKind::SingleChoice => "single choice",
        QuestionKind::MultipleChoice => "multiple choice",
        QuestionKind::FillIn => "fill in",
    };
    let mut prompt = format!("\n❓ {} ({kind_label})\n", question.question);
    for (index, option) in question.options.iter().enumerate() {
        use std::fmt::Write as _;
        let _ = writeln!(prompt, "   {}. {option}", index + 1);
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut read_line = || -> Result<String, ToolError> {
        lines
            .next()
            .transpose()
            .map_err(|error| ToolError::new(format!("reading answer failed: {error}")))?
            .ok_or_else(|| ToolError::new("stdin closed before the question was answered"))
    };

    match question.kind {
        QuestionKind::FillIn => {
            let text = read_line()?;
            let text = text.trim().to_string();
            Ok(AskUserAnswer {
                answers: vec![text.clone()],
                custom: Some(text),
            })
        }
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice => {
            let input = read_line()?;
            let mut answers = Vec::new();
            let mut custom = None;
            for token in input.split(|c: char| c == ',' || c.is_whitespace()) {
                if token.is_empty() {
                    continue;
                }
                if let Ok(number) = token.parse::<usize>()
                    && (1..=question.options.len()).contains(&number)
                {
                    let option = question.options[number - 1].clone();
                    if option == CUSTOM_OPTION {
                        // 自定义选项：二次输入文本（多选只取一次）
                        if custom.is_none() {
                            let text = read_line()?;
                            let text = text.trim().to_string();
                            if !text.is_empty() {
                                custom = Some(text.clone());
                                answers.push(text);
                            }
                        }
                    } else if !answers.contains(&option) {
                        answers.push(option);
                    }
                } else if !answers.iter().any(|answer| answer == token) {
                    // 非编号文本：视为自定义答案
                    custom = Some(token.to_string());
                    answers.push(token.to_string());
                }
            }
            if answers.is_empty() {
                return Err(ToolError::new(
                    "no answer given: enter an option number (or free text)",
                ));
            }
            if question.kind == QuestionKind::SingleChoice && answers.len() > 1 {
                return Err(ToolError::new("single choice accepts exactly one answer"));
            }
            Ok(AskUserAnswer { answers, custom })
        }
    }
}

/// 加载全部 `--image` 附件；任一失败则整体中止（prompt 未发送）。
fn load_images(paths: &[std::path::PathBuf]) -> Result<Vec<nomic_ai::ImageContent>> {
    paths
        .iter()
        .map(|path| crate::images::load_image(path))
        .collect()
}

/// 消费 agent 事件流：落库走 [`SessionRecorder`]（一行接线），流式输出到
/// stdout/stderr。
///
/// 返回运行中见过的 provider 错误（编码在 assistant 消息里）。
async fn drain_events(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    mut recorder: Option<&mut SessionRecorder>,
) -> Option<String> {
    let mut saw_error: Option<String> = None;
    while let Some(event) = events.recv().await {
        // 定稿点落库（父指针推进在 recorder 内）；失败仅告警不中断
        if let Some(recorder) = &mut recorder
            && let Err(error) = recorder.record(&event).await
        {
            eprintln!("\x1b[33m⚠ session 落库失败：{error}\x1b[0m");
        }
        match event {
            AgentEvent::MessageUpdate(AssistantEvent::TextDelta { delta, .. }) => {
                // 锁不跨 await 持有（StdoutLock 非 Send）
                print!("{delta}");
                let _ = std::io::stdout().flush();
            }
            AgentEvent::MessageUpdate(AssistantEvent::ThinkingDelta { delta, .. }) => {
                eprint!("\x1b[2;3m{delta}\x1b[0m");
            }
            AgentEvent::ToolExecutionStart {
                tool_name, args, ..
            } => {
                eprintln!(
                    "\n\x1b[36m▶ {tool_name}\x1b[0m {}",
                    brief_args(&tool_name, &args)
                );
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
            AgentEvent::CompactionStart { tokens_before } => {
                eprintln!("\x1b[2m⟳ 压缩上下文（约 {tokens_before} tokens）…\x1b[0m");
            }
            AgentEvent::CompactionEnd {
                tokens_before,
                kept_count,
                ..
            } => {
                eprintln!(
                    "\x1b[2m✂ 上下文已压缩：约 {tokens_before} tokens → 摘要 + {kept_count} 条近期消息\x1b[0m"
                );
            }
            AgentEvent::MessageEnd { message, .. } => {
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
///
/// 已知工具取关键字段（bash→command，read/write/edit→path，grep/find→pattern），
/// 避免直接展示原始 JSON；未知工具回退为截断 JSON。多行文本压缩为单行。
pub fn brief_args(tool_name: &str, args: &serde_json::Value) -> String {
    const MAX: usize = 120;
    let key_field = match tool_name {
        "bash" => args.get("command").and_then(|v| v.as_str()),
        "read" | "write" | "edit" => args.get("path").and_then(|v| v.as_str()),
        "grep" | "find" => args.get("pattern").and_then(|v| v.as_str()),
        "ask_user_question" => args.get("question").and_then(|v| v.as_str()),
        _ => None,
    };
    let text = match (tool_name, key_field) {
        ("edit", Some(path)) => {
            let count = args
                .get("edits")
                .and_then(|v| v.as_array())
                .map_or(1, Vec::len);
            if count > 1 {
                format!("{path} · {count} 处编辑")
            } else {
                path.to_string()
            }
        }
        (_, Some(field)) => field.to_string(),
        _ => args.to_string(),
    };
    // 压缩空白（含换行），避免多行 command 破坏单行展示
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.len() <= MAX {
        return text;
    }
    let mut index = MAX;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    format!("{}…", &text[..index])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::brief_args;

    #[test]
    fn brief_args_extracts_key_field_per_tool() {
        assert_eq!(
            brief_args("bash", &json!({"command": "ls -la", "timeout": 30})),
            "ls -la"
        );
        assert_eq!(
            brief_args("read", &json!({"path": "src/main.rs", "offset": 10})),
            "src/main.rs"
        );
        assert_eq!(
            brief_args("write", &json!({"path": "a.md", "content": "…"})),
            "a.md"
        );
        assert_eq!(
            brief_args(
                "edit",
                &json!({"path": "a.rs", "edits": [{"oldText": "x", "newText": "y"}]})
            ),
            "a.rs"
        );
        assert_eq!(
            brief_args(
                "edit",
                &json!({"path": "a.rs", "edits": [
                    {"oldText": "x", "newText": "y"},
                    {"oldText": "p", "newText": "q"},
                ]})
            ),
            "a.rs · 2 处编辑"
        );
    }

    #[test]
    fn brief_args_falls_back_to_json_for_unknown_tool() {
        let args = json!({"query": "rust"});
        assert_eq!(brief_args("web_search", &args), args.to_string());
    }

    #[test]
    fn brief_args_squashes_multiline_and_truncates() {
        assert_eq!(
            brief_args("bash", &json!({"command": "cargo build\n  && cargo test"})),
            "cargo build && cargo test"
        );
        let long = "x".repeat(200);
        let summary = brief_args("bash", &json!({"command": long}));
        assert!(summary.ends_with('…'));
        assert!(summary.len() <= 123); // 120 + '…'（3 字节）
    }
}
