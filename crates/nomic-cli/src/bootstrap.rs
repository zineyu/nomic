//! 两种模式共享的运行时初始化：provider/model 解析、stream options、系统提示词、
//! session 新建/恢复。

use std::sync::Arc;

use anyhow::{Context as _, Result};
use nomic_ai::{
    ApiKind, Message, Model, Provider, StreamOptions, ThinkingLevel,
    providers::{AnthropicProvider, OpenAiCompat, OpenAiProvider},
};
use nomic_session::SessionStore;

use crate::Cli;

/// 初始化完成的运行时上下文：构建 agent 所需的全部零件 + 持久化句柄与恢复历史。
pub struct Bootstrap {
    pub model: Model,
    pub provider: Arc<dyn Provider>,
    pub stream_options: StreamOptions,
    pub system_prompt: String,
    /// `Some((store, session_id))` 时开启落库；session 库不可用时降级为 `None`
    pub session: Option<(SessionStore, String)>,
    /// resume 恢复的历史消息（新会话为空）
    pub history: Vec<Message>,
}

/// 按 CLI 参数初始化运行时上下文。
pub async fn bootstrap(cli: &Cli) -> Result<Bootstrap> {
    let provider_kind = cli.provider.clone().unwrap_or_else(|| {
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            "anthropic".to_string()
        } else {
            "openai".to_string()
        }
    });
    let model = resolve_model(&provider_kind, cli);
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
    let system_prompt = build_system_prompt(cli.append_system.as_deref())?;
    let session = init_session(cli).await?;
    let history = session
        .as_ref()
        .map(|init| init.history.clone())
        .unwrap_or_default();
    Ok(Bootstrap {
        model,
        provider,
        stream_options,
        system_prompt,
        session: session.map(|init| (init.store, init.id)),
        history,
    })
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
