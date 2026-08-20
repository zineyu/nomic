//! 两种模式共享的运行时初始化装配：stream options、系统提示词、session
//! 新建/恢复；provider/model 的分层解析在 [`model`][crate::model]。
//!
//! 配置文件存在但非法时硬报错（见 [`config`][crate::config]）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use nomic_ai::{Message, Model, Provider, StreamOptions, ThinkingLevel};
use nomic_prompts::{ProjectDiscovery, PromptResolver, PromptTemplate};
use nomic_session::SessionStore;
use nomic_skills::{ActivatedSkill, SkillResolver};

use crate::Cli;
use crate::config::Config;
use crate::context_files::{ContextFile, discover_agents_files};
use crate::model::{
    ModelResolver, api_key_env, build_provider, cli_model_provider, db_model_history,
    db_reasoning_level, load_catalog_unless_complete, resolve_api_key, select_startup_model,
};

/// 初始化完成的运行时上下文：构建 agent 所需的全部零件 + 持久化句柄与恢复历史。
pub struct Bootstrap {
    pub model: Model,
    /// 运行时模型解析器（TUI `/models` 切换用）：与启动同一分层口径
    pub models: ModelResolver,
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
    /// 可用的 prompt templates（`/name` 调用展开用，已按覆盖规则去重）
    pub prompt_templates: Vec<PromptTemplate>,
    /// 所有可用模型列表（子 agent 模型选择用）
    pub available_models: Vec<Model>,
}

/// 按 CLI 参数与环境初始化运行时上下文。
///
/// provider/model 的选择按 CLI 参数 > sqlite 配置（回退链）解析，两层都没有时
/// 报错（无内置默认模型）；其余可配置项按 CLI 参数 > 环境变量 > 配置文件 >
/// 协议默认 的优先级解析；配置文件存在但非法时硬报错（见 [`config`][crate::config]）。
#[allow(clippy::too_many_lines)]
pub async fn bootstrap(cli: &Cli) -> Result<Bootstrap> {
    let config = crate::config::load()?;
    let env_openai_base_url = std::env::var("OPENAI_BASE_URL").ok();
    // session 库提前打开：模型选择（config 表）与消息持久化共用同一库
    let store = open_store(cli).await?;
    // 数据库中的模型选择历史（最新在前的回退链；库不可用或读取失败为空链）
    let db_history = db_model_history(store.as_ref()).await;
    // catalog 加载提示：CLI 选择器 > 数据库最新选择；都没有时不做完整性预判
    let provider_hint = cli
        .provider
        .clone()
        .or_else(|| cli_model_provider(cli))
        .or_else(|| {
            db_history
                .first()
                .map(|selection| selection.provider.clone())
        });
    let model_id_hint = cli
        .model
        .as_deref()
        .map(|spec| spec.rsplit_once('/').map_or(spec, |(_, model)| model))
        .or_else(|| db_history.first().map(|selection| selection.model.as_str()));
    let catalog =
        load_catalog_unless_complete(config.as_ref(), provider_hint.as_deref(), model_id_hint)
            .await;
    let models = ModelResolver::new(cli, config, env_openai_base_url, catalog);
    let model = select_startup_model(cli, &db_history, &models)?;
    // api_key 显式分层解析（provider 内部的 env 回退发生在请求时，
    // 若把配置文件值直接交给构造器会抢到环境变量前面）。
    let api_key = resolve_api_key(
        cli.api_key.as_deref(),
        std::env::var(api_key_env(model.api)).ok().as_deref(),
        models
            .provider_config(&model.provider)
            .and_then(|p| p.api_key.as_deref()),
        models.config().and_then(|c| c.api_key.as_deref()),
    );
    let provider = build_provider(model.api, api_key.clone());
    // 思考级别恢复链：CLI > config.toml > sqlite 配置表
    let db_reasoning = db_reasoning_level(store.as_ref()).await;
    let reasoning = cli
        .reasoning
        .as_deref()
        .or_else(|| models.config().and_then(|c| c.reasoning.as_deref()))
        .map(parse_reasoning)
        .transpose()?
        .or(db_reasoning);
    let stream_options = StreamOptions {
        temperature: cli
            .temperature
            .or_else(|| models.config().and_then(|c| c.temperature)),
        max_tokens: cli
            .max_tokens
            .or_else(|| models.config().and_then(|c| c.max_tokens)),
        reasoning,
        api_key,
        headers: Vec::new(),
        timeout_ms: None,
    };
    let append_system = cli
        .append_system
        .as_deref()
        .or_else(|| models.config().and_then(|c| c.append_system.as_deref()));
    let cwd = std::env::current_dir().context("get cwd")?;
    let context_files = discover_agents_files(&cwd);
    let skill_resolver = SkillResolver::for_cwd(&cwd).context("初始化 skills 目录失败")?;
    warn_skill_diagnostics(&skill_resolver);
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
    let prompt_templates = load_prompt_templates(cli, &cwd, models.config())?;
    let session = init_session(cli, &cwd, store).await?;
    let history = session
        .as_ref()
        .map(|init| init.history.clone())
        .unwrap_or_default();
    let compaction = compaction_settings(models.config());
    // 所有可用模型列表（子 agent 模型选择用）
    let current_selection = crate::model::ModelSelection {
        provider: model.provider.clone(),
        model: model.id.clone(),
    };
    let available_models = models.all_models(&current_selection);
    Ok(Bootstrap {
        model,
        models,
        provider,
        stream_options,
        system_prompt,
        compaction,
        session: session.map(|init| (init.store, init.id)),
        history,
        skill_resolver,
        prompt_templates,
        available_models,
    })
}

/// 启动时把 skill 加载诊断对用户可见：坏 skill 被静默跳过会让人无从排查。
/// stderr 黄色告警 + tracing 日志（与 session 库告警同一口径）。
fn warn_skill_diagnostics(skill_resolver: &SkillResolver) {
    let catalog = skill_resolver.catalog_with_diagnostics();
    for error in &catalog.errors {
        tracing::warn!(error = %error, "跳过加载失败的 skill");
        eprintln!("\x1b[33m⚠ 跳过加载失败的 skill：{error}\x1b[0m");
    }
}

/// 加载 prompt templates：目录发现（`--no-prompt-templates` 关闭）+ 配置文件
/// `prompts` 与 `--prompt-template` 的显式路径（同名时优先级最高）。
/// 单个模板加载失败只告警不中断（与 skills 同一口径）。
fn load_prompt_templates(
    cli: &Cli,
    cwd: &Path,
    config: Option<&Config>,
) -> Result<Vec<PromptTemplate>> {
    let mut explicit = config
        .and_then(|config| config.prompts.clone())
        .unwrap_or_default();
    explicit.extend(cli.prompt_template.iter().cloned());
    let resolver = if cli.no_prompt_templates {
        PromptResolver::new(
            cwd,
            ProjectDiscovery::Roots(Vec::new()),
            Vec::new(),
            explicit,
        )
    } else {
        PromptResolver::for_cwd(cwd).map(|resolver| resolver.with_explicit(explicit))
    }
    .context("初始化 prompts 目录失败")?;
    let catalog = resolver.catalog_with_diagnostics();
    for error in &catalog.errors {
        tracing::warn!(error = %error, "跳过加载失败的 prompt template");
    }
    Ok(catalog.templates)
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
/// 库不可用（`store` 为 `None`，启动时已告警）时不持久化。
async fn init_session(
    cli: &Cli,
    cwd: &Path,
    store: Option<SessionStore>,
) -> Result<Option<SessionInit>> {
    let Some(store) = store else {
        return Ok(None);
    };
    init_session_in(cli, cwd, store).await
}

/// 打开 session 库：模型选择（config 表）与消息持久化共用同一库。
///
/// resume 语义下打开失败直接报错（用户显式要求恢复）；否则降级为
/// 不持久化（打告警后返回 `None`），不阻断本次运行。
async fn open_store(cli: &Cli) -> Result<Option<SessionStore>> {
    let resume = cli.continue_session || cli.session.is_some();
    match SessionStore::open_default().await {
        Ok(store) => Ok(Some(store)),
        Err(error) => {
            if resume {
                return Err(error).context("打开 session 库失败，无法恢复会话");
            }
            eprintln!("\x1b[33m⚠ 打开 session 库失败，本次运行不持久化：{error}\x1b[0m");
            Ok(None)
        }
    }
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
        .find(|summary| normalize_path(&summary.workspace) == target)
        .map(|summary| summary.id)
        .with_context(|| {
            format!(
                "当前目录 {} 没有可恢复的 session\
                 （用 `nomic resume` 交互选择任意目录的 session）",
                cwd.display()
            )
        })
}

/// 加载指定 session 的历史消息。
async fn load_history(store: &SessionStore, id: &str) -> Result<Vec<Message>> {
    store
        .load_messages(id)
        .await
        .with_context(|| "加载 session 历史失败".to_string())
}

/// 显式 `--session` 恢复的 session 属于其他目录时提示（不阻断）。
async fn warn_if_cross_cwd(store: &SessionStore, id: &str, cwd: &Path) {
    let Ok(sessions) = store.list_sessions().await else {
        return;
    };
    if let Some(summary) = sessions.iter().find(|s| s.id == id)
        && normalize_path(&summary.workspace) != normalize_path(cwd)
    {
        eprintln!(
            "\x1b[33m⚠ session 属于 {}，与当前目录不同\x1b[0m",
            summary.workspace.display()
        );
    }
}

/// 路径规范化：优先 canonicalize（解析符号链接），路径不存在时退回原始路径。
fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn parse_reasoning(level: &str) -> Result<ThinkingLevel> {
    match level {
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        _ => bail!("--reasoning 取值非法：{level:?}（可选 minimal / low / medium / high）"),
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
         - read: Read file contents and skill://<name>[/<path>] instructions and resources\n\
         - bash: Execute bash commands\n\
         - grep: Search file contents with a regex (ripgrep-style)\n\
         - find: Find files and directories by glob pattern (fd-style)\n\
         - edit: Make precise file edits with exact text replacement\n\
         - write: Create or overwrite files\n\n\
         Guidelines:\n\
         - Use grep to search file contents and find to locate files\n\
         - Use bash for other shell commands (cargo, git, jj, ls, etc.)\n\
         - Use read to examine files instead of cat or sed\n\
         - Skills are reusable instruction documents; read skill://<name> before following one\n\
         - skill://<name>/<path> reads supporting files (scripts/, references/, etc.) inside the skill directory\n\
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
        assert!(message.contains("nomic resume"), "{message}");
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
        // 注入块带 skill 根目录指引（相对路径解析基准）
        assert!(prompt.contains(&format!("[Skill directory: {}", root.display())));
    }

    // ── --reasoning 取值 ────────────────────────────────────────────────

    #[test]
    fn cli_reasoning_rejects_invalid_levels() {
        assert_eq!(parse_reasoning("low").expect("low"), ThinkingLevel::Low);
        assert_eq!(
            parse_reasoning("medium").expect("medium"),
            ThinkingLevel::Medium
        );
        assert!(parse_reasoning("extreme").is_err(), "非法级别必须报错");
    }
}
