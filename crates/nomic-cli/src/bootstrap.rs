//! 两种模式共享的运行时初始化：provider/model 解析、stream options、系统提示词、
//! session 新建/恢复。
//!
//! provider / base_url / api_key 等连接参数按 CLI 参数 > 环境变量 >
//! `providers.<名字>.*` > 平铺配置 > 内置默认 解析（永远来自用户指定）；
//! 模型规格字段（展示名、推理能力、上下文/输出上限、费率）逐字段按
//! 配置 `providers.<名字>.models.<模型id>` > models.dev > 内置预设 解析。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use nomic_ai::{
    ApiKind, Catalog, Message, Model, ModelSpec, Provider, StreamOptions, ThinkingLevel,
    providers::{AnthropicProvider, OpenAiCompat, OpenAiProvider},
};
use nomic_session::SessionStore;
use nomic_skills::{ActivatedSkill, SkillResolver};

use crate::Cli;
use crate::config::Config;
use crate::context_files::{ContextFile, discover_agents_files};

/// 初始化完成的运行时上下文：构建 agent 所需的全部零件 + 持久化句柄与恢复历史。
pub struct Bootstrap {
    pub model: Model,
    pub provider: Arc<dyn Provider>,
    pub stream_options: StreamOptions,
    pub system_prompt: String,
    /// 上下文压缩配置（`[compaction]` 合并内置默认）
    pub compaction: nomic_core::CompactionSettings,
    /// `Some((store, session_id))` 时开启落库；session 库不可用时降级为 `None`
    pub session: Option<(SessionStore, String)>,
    /// resume 恢复的历史消息（新会话为空）
    pub history: Vec<Message>,
    /// skill 解析器（同时注入 read 工具）
    pub skill_resolver: SkillResolver,
}

/// 按 CLI 参数与环境初始化运行时上下文。
///
/// 可配置项统一按 CLI 参数 > 环境变量 > 配置文件 > 内置默认 的优先级解析；
/// 配置文件存在但非法时硬报错（见 [`config`][crate::config]）。
pub async fn bootstrap(cli: &Cli) -> Result<Bootstrap> {
    let config = crate::config::load()?;
    let env_openai_base_url = std::env::var("OPENAI_BASE_URL").ok();
    let provider_kind = cli
        .provider
        .clone()
        .or_else(|| config.as_ref().and_then(|c| c.provider.clone()))
        .unwrap_or_else(|| {
            if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                "anthropic".to_string()
            } else {
                "openai".to_string()
            }
        });
    let model_id_hint = cli
        .model
        .clone()
        .or_else(|| config.as_ref().and_then(|c| c.model.clone()));
    let catalog =
        load_catalog_unless_complete(config.as_ref(), &provider_kind, model_id_hint.as_deref())
            .await;
    let model = resolve_model(
        &provider_kind,
        cli,
        config.as_ref(),
        env_openai_base_url.as_deref(),
        catalog.as_ref(),
    )?;
    // api_key 显式分层解析（provider 内部的 env 回退发生在请求时，
    // 若把配置文件值直接交给构造器会抢到环境变量前面）。
    let api_key = resolve_api_key(
        cli.api_key.as_deref(),
        std::env::var(api_key_env(model.api)).ok().as_deref(),
        provider_config(config.as_ref(), &provider_kind).and_then(|p| p.api_key.as_deref()),
        config.as_ref().and_then(|c| c.api_key.as_deref()),
    );
    let provider: Arc<dyn Provider> = match model.api {
        ApiKind::AnthropicMessages => Arc::new(AnthropicProvider::new(api_key.clone())),
        ApiKind::OpenAiCompletions => Arc::new(OpenAiProvider::new(
            api_key.clone(),
            OpenAiCompat::default(),
        )),
    };
    let stream_options = StreamOptions {
        temperature: cli
            .temperature
            .or_else(|| config.as_ref().and_then(|c| c.temperature)),
        max_tokens: cli
            .max_tokens
            .or_else(|| config.as_ref().and_then(|c| c.max_tokens)),
        reasoning: cli
            .reasoning
            .as_deref()
            .or_else(|| config.as_ref().and_then(|c| c.reasoning.as_deref()))
            .map(parse_reasoning),
        api_key,
        headers: Vec::new(),
        timeout_ms: None,
    };
    let append_system = cli
        .append_system
        .as_deref()
        .or_else(|| config.as_ref().and_then(|c| c.append_system.as_deref()));
    let cwd = std::env::current_dir().context("get cwd")?;
    let context_files = discover_agents_files(&cwd);
    let skill_resolver = SkillResolver::for_cwd(&cwd).context("初始化 skills 目录失败")?;
    let active_skills = cli
        .skill
        .iter()
        .map(|name| {
            skill_resolver
                .activate(name)
                .with_context(|| format!("激活 skill {name:?} 失败"))
        })
        .collect::<Result<Vec<_>>>()?;
    let system_prompt = build_system_prompt(
        &cwd,
        append_system,
        &context_files,
        &skill_resolver,
        &active_skills,
    );
    let session = init_session(cli, &cwd).await?;
    let history = session
        .as_ref()
        .map(|init| init.history.clone())
        .unwrap_or_default();
    Ok(Bootstrap {
        model,
        provider,
        stream_options,
        system_prompt,
        compaction: compaction_settings(config.as_ref()),
        session: session.map(|init| (init.store, init.id)),
        history,
        skill_resolver,
    })
}

/// 解析压缩配置：`[compaction]` 表逐字段合并内置默认。
fn compaction_settings(config: Option<&Config>) -> nomic_core::CompactionSettings {
    config.and_then(|c| c.compaction.as_ref()).map_or_else(
        nomic_core::CompactionSettings::default,
        crate::config::CompactionConfig::settings,
    )
}

/// session 初始化结果：store、session id 与恢复的历史消息（新会话为空）。
#[derive(Debug)]
struct SessionInit {
    store: SessionStore,
    id: String,
    history: Vec<Message>,
}

/// 初始化 session：按 `--continue`/`--session` 恢复既有会话，否则新建。
///
/// - resume 语义下打开库/加载消息失败直接报错（用户显式要求恢复）
/// - 新会话语义下降级为不持久化（打告警后返回 `None`），不阻断本次运行
async fn init_session(cli: &Cli, cwd: &Path) -> Result<Option<SessionInit>> {
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
    init_session_in(cli, cwd, store).await
}

/// 在指定 cwd 与 store 下初始化 session（`init_session` 的可测试内核）。
async fn init_session_in(
    cli: &Cli,
    cwd: &Path,
    store: SessionStore,
) -> Result<Option<SessionInit>> {
    if cli.continue_session {
        // --continue 只自动恢复当前目录的 session：跨项目恢复会把 A 项目的
        // 对话历史带入 B 项目的工具执行环境，是明确的误操作风险。
        let id = latest_session_in(&store, cwd).await?;
        let history = load_history(&store, &id).await?;
        return Ok(Some(SessionInit { store, id, history }));
    }
    if let Some(id) = &cli.session {
        // 显式 --session 尊重用户意图，可跨目录恢复，但跨目录时提示
        warn_if_cross_cwd(&store, id, cwd).await;
        let history = load_history(&store, id).await?;
        return Ok(Some(SessionInit {
            store,
            id: id.clone(),
            history,
        }));
    }

    match store.create_session(cwd).await {
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

/// 当前规范化 cwd 下最近活跃的 session id。
async fn latest_session_in(store: &SessionStore, cwd: &Path) -> Result<String> {
    let target = normalize_path(cwd);
    let sessions = store.list_sessions().await.context("列出 session 失败")?;
    sessions
        .into_iter()
        .find(|summary| normalize_path(&summary.cwd) == target)
        .map(|summary| summary.id)
        .with_context(|| {
            format!(
                "当前目录 {} 没有可恢复的 session\
                 （用 `nomic sessions list` 查看全部，或用 --session <ID> 指定）",
                cwd.display()
            )
        })
}

/// 加载指定 session 的历史消息。
async fn load_history(store: &SessionStore, id: &str) -> Result<Vec<Message>> {
    store
        .load_messages(id)
        .await
        .with_context(|| format!("加载 session {id} 失败"))
}

/// 显式 `--session` 恢复的 session 属于其他目录时提示（不阻断）。
async fn warn_if_cross_cwd(store: &SessionStore, id: &str, cwd: &Path) {
    let Ok(sessions) = store.list_sessions().await else {
        return;
    };
    if let Some(summary) = sessions.iter().find(|s| s.id == id)
        && normalize_path(&summary.cwd) != normalize_path(cwd)
    {
        eprintln!(
            "\x1b[33m⚠ session 属于 {}，与当前目录不同\x1b[0m",
            summary.cwd.display()
        );
    }
}

/// 路径规范化：优先 canonicalize（解析符号链接），路径不存在时退回原始路径。
fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// provider 各 API 家族对应的环境变量名（`api_key` 分层解析用）。
const fn api_key_env(api: ApiKind) -> &'static str {
    match api {
        ApiKind::AnthropicMessages => "ANTHROPIC_API_KEY",
        ApiKind::OpenAiCompletions => "OPENAI_API_KEY",
    }
}

/// 解析 api_key：CLI 参数 > 环境变量 > `providers.<名字>.api_key` > 平铺配置文件。
fn resolve_api_key(
    cli: Option<&str>,
    env: Option<&str>,
    provider: Option<&str>,
    config: Option<&str>,
) -> Option<String> {
    cli.or(env).or(provider).or(config).map(str::to_string)
}

/// 取 `providers.<名字>` 定义。
fn provider_config<'c>(
    config: Option<&'c Config>,
    provider_kind: &str,
) -> Option<&'c crate::config::ProviderConfig> {
    config
        .and_then(|c| c.providers.as_ref())
        .and_then(|providers| providers.get(provider_kind))
}

/// 取配置中 `providers.<名字>.models.<模型id>` 的规格覆盖。
fn model_spec_from_config<'c>(
    config: Option<&'c Config>,
    provider_kind: &str,
    model_id: Option<&str>,
) -> Option<&'c ModelSpec> {
    provider_config(config, provider_kind)
        .and_then(|p| p.models.as_ref())
        .and_then(|models| model_id.and_then(|id| models.get(id)))
}

/// 加载 models.dev 目录；配置已给全规格字段时跳过（不读缓存、不联网），
/// 目录不可用时告警并返回 `None`（调用方落到内置预设）。
async fn load_catalog_unless_complete(
    config: Option<&Config>,
    provider_kind: &str,
    model_id_hint: Option<&str>,
) -> Option<Catalog> {
    let complete = model_spec_from_config(config, provider_kind, model_id_hint)
        .is_some_and(ModelSpec::is_complete);
    if complete {
        return None;
    }
    let catalog = nomic_ai::models_dev::load().await;
    if catalog.is_none() {
        eprintln!("\x1b[33m⚠ models.dev 目录不可用，模型规格回落到内置默认值\x1b[0m");
    }
    catalog
}

/// provider 内置预设：分层解析的最底层（「全局默认值」）。
struct Preset {
    /// 默认模型 id（自定义 provider 无内置默认，必须显式指定）
    default_model: Option<&'static str>,
    /// 默认 base URL
    default_base_url: &'static str,
    /// 规格默认值（除 `name` 外全字段有值；`name` 缺省回退为模型 id）
    spec: ModelSpec,
}

/// 内置 provider（anthropic / openai）的预设。
fn builtin_preset(provider_kind: &str) -> Option<Preset> {
    match provider_kind {
        "anthropic" => Some(Preset {
            default_model: Some("claude-sonnet-4-5"),
            default_base_url: "https://api.anthropic.com",
            spec: ModelSpec {
                name: None,
                reasoning: Some(true),
                context_window: Some(200_000),
                max_tokens: Some(64_000),
                cost_input: Some(3.0),
                cost_output: Some(15.0),
                cost_cache_read: Some(0.3),
                cost_cache_write: Some(3.75),
            },
        }),
        "openai" => Some(Preset {
            default_model: Some("gpt-5.2"),
            default_base_url: "https://api.openai.com/v1",
            spec: ModelSpec {
                name: None,
                reasoning: Some(true),
                context_window: Some(400_000),
                max_tokens: Some(128_000),
                cost_input: Some(0.0),
                cost_output: Some(0.0),
                cost_cache_read: Some(0.0),
                cost_cache_write: Some(0.0),
            },
        }),
        _ => None,
    }
}

/// 自定义 provider 的中性预设：无默认模型，规格字段全为保守值。
const fn neutral_preset(api: ApiKind) -> Preset {
    Preset {
        default_model: None,
        default_base_url: match api {
            ApiKind::AnthropicMessages => "https://api.anthropic.com",
            ApiKind::OpenAiCompletions => "https://api.openai.com/v1",
        },
        spec: ModelSpec {
            name: None,
            reasoning: Some(false),
            context_window: Some(0),
            max_tokens: Some(0),
            cost_input: Some(0.0),
            cost_output: Some(0.0),
            cost_cache_read: Some(0.0),
            cost_cache_write: Some(0.0),
        },
    }
}

/// 解析模型。
///
/// - `provider` / `base_url` 永远来自用户指定：CLI 参数 > 环境变量 >
///   `providers.<名字>.*` > 平铺配置 > 内置默认，不经由 models.dev
/// - 规格字段（`name` / `reasoning` / `context_window` / `max_tokens` / `cost_*`）
///   逐字段分层：配置 `providers.<名字>.models.<模型id>` > models.dev > 内置预设
fn resolve_model(
    provider_kind: &str,
    cli: &Cli,
    config: Option<&Config>,
    env_openai_base_url: Option<&str>,
    catalog: Option<&Catalog>,
) -> Result<Model> {
    let provider_config = provider_config(config, provider_kind);
    let api = provider_config
        .and_then(|p| p.api)
        .or_else(|| crate::config::infer_api(provider_kind))
        .with_context(|| {
            format!(
                "未知 provider {provider_kind:?}：请使用 anthropic / openai，\
                 或在 config.toml 的 [providers.{provider_kind}] 中定义并指定 api"
            )
        })?;
    let preset = builtin_preset(provider_kind).unwrap_or_else(|| neutral_preset(api));
    let base_url = cli
        .base_url
        .clone()
        .or_else(|| {
            env_openai_base_url
                .filter(|_| api == ApiKind::OpenAiCompletions)
                .map(str::to_string)
        })
        .or_else(|| provider_config.and_then(|p| p.base_url.clone()))
        .or_else(|| config.and_then(|c| c.base_url.clone()))
        .unwrap_or_else(|| preset.default_base_url.to_string());
    let id = cli
        .model
        .clone()
        .or_else(|| config.and_then(|c| c.model.clone()))
        .or_else(|| preset.default_model.map(str::to_string))
        .with_context(|| {
            format!("provider {provider_kind:?} 没有内置默认模型，请用 --model 或配置 model 指定")
        })?;
    let spec = model_spec_from_config(config, provider_kind, Some(&id))
        .cloned()
        .unwrap_or_default()
        .or_fill(
            &catalog
                .and_then(|c| c.lookup(Some(provider_kind), &id))
                .cloned()
                .unwrap_or_default(),
        )
        .or_fill(&preset.spec);
    Ok(Model {
        name: spec.name.unwrap_or_else(|| id.clone()),
        id,
        api,
        provider: provider_kind.to_string(),
        base_url,
        reasoning: spec.reasoning.unwrap_or(false),
        context_window: spec.context_window.unwrap_or(0),
        max_tokens: spec.max_tokens.unwrap_or(0),
        cost_input: spec.cost_input.unwrap_or(0.0),
        cost_output: spec.cost_output.unwrap_or(0.0),
        cost_cache_read: spec.cost_cache_read.unwrap_or(0.0),
        cost_cache_write: spec.cost_cache_write.unwrap_or(0.0),
    })
}

fn parse_reasoning(level: &str) -> ThinkingLevel {
    match level {
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        _ => ThinkingLevel::High,
    }
}

/// 系统提示词（对齐 pi 的结构与措辞）：基础契约 → AGENTS.md（根到叶）→
/// `append_system` → 当前工作目录脚注。
fn build_system_prompt(
    cwd: &Path,
    append: Option<&str>,
    context_files: &[ContextFile],
    skill_resolver: &SkillResolver,
    active_skills: &[ActivatedSkill],
) -> String {
    let mut prompt = "You are an expert coding assistant operating inside nomic, a coding agent harness. \
         You help users by reading files, executing commands, editing code, and writing new files.\n\
         Available tools:\n\
         - read: Read file contents and skill://<name> instructions\n\
         - bash: Execute bash commands (ls, rg, find, etc.)\n\
         - edit: Make precise file edits with exact text replacement\n\
         - write: Create or overwrite files\n\n\
         Guidelines:\n\
         - Use bash for file operations like ls, rg, find\n\
         - Use read to examine files instead of cat or sed\n\
         - Skills are reusable instruction documents; read skill://<name> before following one\n\
         - Do not write or edit skill:// resources; edit their backing files only when the user asks\n\
         - Be concise in your responses\n\
         - Show file paths clearly when working with files"
        .to_string();
    for file in context_files {
        use std::fmt::Write as _;
        let _ = write!(
            prompt,
            "\n\n<project_instructions path=\"{}\">\n{}\n</project_instructions>",
            file.path.display(),
            file.content.trim_end()
        );
    }
    if let Some(catalog) = skill_resolver.prompt_catalog() {
        prompt.push_str("\n\n<available_skills>\n");
        prompt.push_str(&catalog);
        prompt.push_str("\n</available_skills>");
    }
    for skill in active_skills {
        prompt.push_str("\n\n");
        prompt.push_str(&skill.prompt_tag());
    }
    if let Some(extra) = append {
        prompt.push_str("\n\n");
        prompt.push_str(extra);
    }
    {
        use std::fmt::Write as _;
        let _ = write!(prompt, "\n\nCurrent working directory: {}", cwd.display());
    }
    prompt
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use nomic_ai::{UserMessage, UserMessageContent};

    use super::*;

    /// 从 argv 构造 Cli（与真实命令行解析路径一致）。
    fn cli(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("nomic").chain(args.iter().copied()))
    }

    fn user_message(text: &str, timestamp: u64) -> Message {
        Message::User(UserMessage {
            content: UserMessageContent::Text(text.to_string()),
            timestamp,
        })
    }

    // ── session：--continue 的 cwd 隔离 ─────────────────────────────────────

    #[tokio::test]
    async fn continue_resumes_latest_session_in_current_cwd() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        let dir_b = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::in_memory().await.expect("store");
        let session_b = store.create_session(dir_b.path()).await.expect("create b");
        store
            .append_message(&session_b, None, &user_message("from b", 1000))
            .await
            .expect("append b");
        // A 的消息更新：全局最近；但在 B 目录 --continue 仍必须选 B
        let session_a = store.create_session(dir_a.path()).await.expect("create a");
        store
            .append_message(&session_a, None, &user_message("from a", 2000))
            .await
            .expect("append a");

        let init = init_session_in(&cli(&["--continue"]), dir_b.path(), store)
            .await
            .expect("init")
            .expect("session");
        assert_eq!(init.id, session_b);
        assert_eq!(init.history.len(), 1);
    }

    #[tokio::test]
    async fn continue_fails_when_current_cwd_has_no_session() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        let dir_b = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::in_memory().await.expect("store");
        store.create_session(dir_a.path()).await.expect("create");

        let error = init_session_in(&cli(&["--continue"]), dir_b.path(), store)
            .await
            .expect_err("当前目录无 session 时必须报错");
        let message = format!("{error:#}");
        assert!(message.contains("没有可恢复的 session"), "{message}");
        assert!(message.contains("--session"), "{message}");
    }

    #[tokio::test]
    async fn explicit_session_resumes_across_cwd() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        let dir_b = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::in_memory().await.expect("store");
        let id = store.create_session(dir_a.path()).await.expect("create");
        store
            .append_message(&id, None, &user_message("hello", 1000))
            .await
            .expect("append");

        let init = init_session_in(&cli(&["--session", &id]), dir_b.path(), store)
            .await
            .expect("init")
            .expect("session");
        assert_eq!(init.id, id);
        assert_eq!(init.history.len(), 1);
    }

    // ── session：创建 → 落库 → 恢复 roundtrip ────────────────────────────────

    #[tokio::test]
    async fn create_append_resume_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::in_memory().await.expect("store");
        let created = init_session_in(&cli(&[]), dir.path(), store.clone())
            .await
            .expect("init")
            .expect("session");
        assert!(created.history.is_empty(), "新 session 无历史");

        created
            .store
            .append_message(&created.id, None, &user_message("hi", 1000))
            .await
            .expect("append");

        let resumed = init_session_in(&cli(&["--continue"]), dir.path(), store)
            .await
            .expect("init")
            .expect("session");
        assert_eq!(resumed.id, created.id);
        assert_eq!(resumed.history.len(), 1);
    }

    // ── 系统提示词：AGENTS.md 注入 ──────────────────────────────────────

    fn context_file(path: &str, content: &str) -> ContextFile {
        ContextFile {
            path: PathBuf::from(path),
            content: content.to_string(),
        }
    }

    fn empty_skill_resolver() -> SkillResolver {
        SkillResolver::new(
            Path::new("/repo"),
            nomic_skills::ProjectDiscovery::Roots(Vec::new()),
            Vec::new(),
        )
        .expect("empty skill resolver")
    }

    #[test]
    fn prompt_injects_context_files_root_to_leaf() {
        let files = [
            context_file("/repo/AGENTS.md", "root rules"),
            context_file("/repo/sub/AGENTS.md", "sub rules"),
        ];
        let prompt = build_system_prompt(
            Path::new("/repo/sub"),
            None,
            &files,
            &empty_skill_resolver(),
            &[],
        );

        let root_at = prompt.find("root rules").expect("root 内容");
        let sub_at = prompt.find("sub rules").expect("sub 内容");
        assert!(root_at < sub_at, "根到叶顺序");
        // 每份文件都带绝对路径标签
        assert!(prompt.contains("<project_instructions path=\"/repo/AGENTS.md\">"));
        assert!(prompt.contains("<project_instructions path=\"/repo/sub/AGENTS.md\">"));
        assert_eq!(prompt.matches("</project_instructions>").count(), 2);
    }

    #[test]
    fn prompt_keeps_append_and_cwd_without_context_files() {
        let prompt = build_system_prompt(
            Path::new("/repo"),
            Some("额外指令"),
            &[],
            &empty_skill_resolver(),
            &[],
        );
        assert!(!prompt.contains("project_instructions"));
        assert!(prompt.contains("额外指令"));
        assert!(prompt.contains("Current working directory: /repo"));
    }

    #[test]
    fn prompt_orders_base_context_append_cwd() {
        let files = [context_file("/repo/AGENTS.md", "root rules")];
        let prompt = build_system_prompt(
            Path::new("/repo"),
            Some("额外指令"),
            &files,
            &empty_skill_resolver(),
            &[],
        );
        let base_at = prompt.find("Available tools").expect("base");
        let ctx_at = prompt.find("root rules").expect("context");
        let append_at = prompt.find("额外指令").expect("append");
        let cwd_at = prompt.find("Current working directory").expect("cwd");
        assert!(base_at < ctx_at && ctx_at < append_at && append_at < cwd_at);
    }

    #[test]
    fn prompt_injects_skill_catalog_and_active_skill() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("skills").join("rust-review");
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(
            root.join("SKILL.md"),
            "---\ndescription: Review Rust code\ntriggers: [rust, review]\n---\n# Review\nCheck unsafe code.",
        )
        .expect("write skill");
        let resolver = SkillResolver::new(
            dir.path(),
            nomic_skills::ProjectDiscovery::Roots(Vec::new()),
            vec![nomic_skills::SkillRoot {
                path: dir.path().join("skills"),
                scope: nomic_skills::SkillScope::Project,
            }],
        )
        .expect("resolver");
        let active = resolver.activate("rust-review").expect("activate");

        let prompt = build_system_prompt(
            dir.path(),
            None,
            &[],
            &resolver,
            std::slice::from_ref(&active),
        );
        assert!(prompt.contains("<available_skills>"));
        assert!(prompt.contains("skill://rust-review"));
        assert!(prompt.contains("Review Rust code"));
        assert!(prompt.contains("triggers: rust, review"));
        assert!(prompt.contains("<active_skill name=\"rust-review\""));
        assert!(prompt.contains("Check unsafe code."));
    }

    // ── 配置分层：CLI > 环境变量 > 配置文件 > 内置默认 ───────────────────────

    /// 裁剪的 models.dev api.json fixture。
    const MODELS_DEV_FIXTURE: &str = r#"{
        "deepseek": {
            "id": "deepseek",
            "models": {
                "deepseek-chat": {
                    "id": "deepseek-chat",
                    "name": "DeepSeek Chat",
                    "reasoning": false,
                    "limit": { "context": 128000, "output": 8192 },
                    "cost": { "input": 0.27, "output": 1.1, "cache_read": 0.07, "cache_write": 0.27 }
                }
            }
        },
        "openai": {
            "id": "openai",
            "models": {
                "gpt-5.2": {
                    "id": "gpt-5.2",
                    "name": "GPT-5.2",
                    "reasoning": true,
                    "limit": { "context": 400000, "output": 128000 }
                }
            }
        }
    }"#;

    fn catalog() -> Catalog {
        Catalog::parse(MODELS_DEV_FIXTURE).expect("catalog fixture")
    }

    fn resolve(
        provider_kind: &str,
        cli: &Cli,
        config: Option<&Config>,
        env_openai_base_url: Option<&str>,
        catalog: Option<&Catalog>,
    ) -> Model {
        resolve_model(provider_kind, cli, config, env_openai_base_url, catalog).expect("resolve")
    }

    #[test]
    fn model_prefers_cli_over_config() {
        let cli = cli(&["--model", "cli-model"]);
        let config = Config {
            model: Some("config-model".to_string()),
            ..Config::default()
        };
        let model = resolve("openai", &cli, Some(&config), None, None);
        assert_eq!(model.id, "cli-model");
    }

    #[test]
    fn base_url_precedence_cli_env_config_default() {
        let config = Config {
            base_url: Some("https://config".to_string()),
            ..Config::default()
        };
        let with_flag = cli(&["--base-url", "https://cli"]);
        let plain = cli(&[]);
        // CLI 参数最高
        let model = resolve(
            "openai",
            &with_flag,
            Some(&config),
            Some("https://env"),
            None,
        );
        assert_eq!(model.base_url, "https://cli");
        // 环境变量次之
        let model = resolve("openai", &plain, Some(&config), Some("https://env"), None);
        assert_eq!(model.base_url, "https://env");
        // 配置文件再次
        let model = resolve("openai", &plain, Some(&config), None, None);
        assert_eq!(model.base_url, "https://config");
        // 内置默认兜底
        let model = resolve("openai", &plain, None, None, None);
        assert_eq!(model.base_url, "https://api.openai.com/v1");
        // OPENAI_BASE_URL 对 anthropic 不生效
        let model = resolve("anthropic", &plain, None, Some("https://env"), None);
        assert_eq!(model.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn provider_table_base_url_beats_flat_config() {
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openai".to_string(),
            crate::config::ProviderConfig {
                base_url: Some("https://provider-table".to_string()),
                ..crate::config::ProviderConfig::default()
            },
        );
        let config = Config {
            base_url: Some("https://flat".to_string()),
            providers: Some(providers),
            ..Config::default()
        };
        let model = resolve("openai", &cli(&[]), Some(&config), None, None);
        assert_eq!(model.base_url, "https://provider-table");
    }

    #[test]
    fn api_key_precedence_cli_env_provider_config() {
        let key = resolve_api_key(Some("cli"), Some("env"), Some("provider"), Some("config"));
        assert_eq!(key.as_deref(), Some("cli"));
        let key = resolve_api_key(None, Some("env"), Some("provider"), Some("config"));
        assert_eq!(key.as_deref(), Some("env"));
        let key = resolve_api_key(None, None, Some("provider"), Some("config"));
        assert_eq!(key.as_deref(), Some("provider"));
        let key = resolve_api_key(None, None, None, Some("config"));
        assert_eq!(key.as_deref(), Some("config"));
        assert_eq!(resolve_api_key(None, None, None, None), None);
    }

    // ── 规格字段分层：配置 > models.dev > 内置预设 ──────────────────────────

    #[test]
    fn spec_from_catalog_fills_fields_preset_is_last_resort() {
        let plain = cli(&[]);
        // models.dev 命中：gpt-5.2 有 limit 但无 cost → cost 落预设（openai 预设为 0）
        let model = resolve("openai", &plain, None, None, Some(&catalog()));
        assert_eq!(model.id, "gpt-5.2");
        assert_eq!(model.name, "GPT-5.2", "展示名来自 models.dev");
        assert_eq!(model.context_window, 400_000);
        assert_eq!(model.max_tokens, 128_000);
        assert!(model.reasoning);
        assert_eq!(
            Some(model.cost_input),
            Some(0.0),
            "models.dev 缺 cost 时落预设"
        );
        // 无 models.dev：保持今天的内置默认行为
        let model = resolve("openai", &plain, None, None, None);
        assert_eq!(model.name, "gpt-5.2", "name 兜底为模型 id");
        assert_eq!(model.context_window, 400_000);
        let model = resolve("anthropic", &plain, None, None, None);
        assert_eq!(model.context_window, 200_000);
        assert_eq!(Some(model.cost_input), Some(3.0));
    }

    #[test]
    fn config_spec_overrides_catalog_per_field() {
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "gpt-5.2".to_string(),
            ModelSpec {
                max_tokens: Some(8192),
                ..ModelSpec::default()
            },
        );
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openai".to_string(),
            crate::config::ProviderConfig {
                models: Some(models),
                ..crate::config::ProviderConfig::default()
            },
        );
        let config = Config {
            providers: Some(providers),
            ..Config::default()
        };
        let model = resolve("openai", &cli(&[]), Some(&config), None, Some(&catalog()));
        assert_eq!(model.max_tokens, 8192, "配置覆盖 models.dev");
        assert_eq!(model.context_window, 400_000, "未覆盖字段仍来自 models.dev");
        assert_eq!(model.name, "GPT-5.2");
    }

    // ── 自定义 provider ────────────────────────────────────────────────────

    fn deepseek_config() -> Config {
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "deepseek".to_string(),
            crate::config::ProviderConfig {
                api: Some(ApiKind::OpenAiCompletions),
                base_url: Some("https://api.deepseek.com/v1".to_string()),
                api_key: Some("sk-deepseek".to_string()),
                models: None,
            },
        );
        Config {
            model: Some("deepseek-chat".to_string()),
            providers: Some(providers),
            ..Config::default()
        }
    }

    #[test]
    fn custom_provider_resolves_via_config_and_global_catalog_scan() {
        let config = deepseek_config();
        // 即便 provider 名不是 models.dev 的一级键，也按模型 id 全局扫描命中
        let model = resolve("deepseek", &cli(&[]), Some(&config), None, Some(&catalog()));
        assert_eq!(model.api, ApiKind::OpenAiCompletions);
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.base_url, "https://api.deepseek.com/v1");
        assert_eq!(model.name, "DeepSeek Chat");
        assert_eq!(model.context_window, 128_000);
        assert_eq!(Some(model.cost_output), Some(1.1));
        assert!(!model.reasoning);
    }

    #[test]
    fn custom_provider_without_catalog_uses_neutral_defaults() {
        let config = deepseek_config();
        let model = resolve("deepseek", &cli(&[]), Some(&config), None, None);
        assert_eq!(model.name, "deepseek-chat");
        assert_eq!(model.context_window, 0);
        assert!(!model.reasoning);
        assert_eq!(Some(model.cost_input), Some(0.0));
    }

    #[test]
    fn custom_provider_requires_explicit_model() {
        let mut config = deepseek_config();
        config.model = None;
        let error = resolve_model("deepseek", &cli(&[]), Some(&config), None, None)
            .expect_err("自定义 provider 无默认模型，必须显式指定");
        assert!(format!("{error:#}").contains("没有内置默认模型"));
    }

    #[test]
    fn unknown_provider_requires_config_definition() {
        let error = resolve_model("gemini", &cli(&[]), None, None, None)
            .expect_err("未知 provider 必须报错");
        assert!(format!("{error:#}").contains("未知 provider"));
    }
}
