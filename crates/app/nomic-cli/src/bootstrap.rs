//! 两种模式共享的运行时初始化装配：stream options、系统提示词、session
//! 新建/恢复；provider/model 的分层解析在 [`model`][crate::model]。
//!
//! 设置来自 sqlite（providers / model_specs / settings 三表快照，ADR-0039），
//! 不再读取配置文件。装配产物注册为进程级应用状态（[`AppState`]，
//! ADR-0047）：全部组件作为服务进入 state，由各入口与内部组件提取。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use nomic_ai::{Message, Model, StreamOptions, ThinkingLevel};
use nomic_prompts::{ProjectDiscovery, PromptResolver, PromptTemplate};
use nomic_session::SessionStore;
use nomic_skills::SkillResolver;

use crate::Cli;
use crate::model::{
    ModelResolver, api_key_env, cli_model_provider, db_model_history, db_reasoning_level,
    load_catalog_unless_complete, resolve_api_key, select_startup_model,
};
use crate::settings::Settings;
use crate::state::{AppState, EventBus, Services};

pub use crate::system_prompt::SystemPromptRecipe;

/// session 初始化策略：交互/print 模式启动即建/恢复 session；web 模式只开库，
/// session 由前端按 project 显式创建（无默认 project，见 ADR-0030）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPolicy {
    /// 启动即初始化 session（新建或按 --continue/--session 恢复）
    Init,
    /// 只打开库，不创建/恢复 session（web 模式：无默认 project）
    OpenStoreOnly,
}

/// 按 CLI 参数与环境初始化运行时状态（ADR-0047 的唯一真实装配点）。
///
/// provider/model 的选择按 CLI 参数 > sqlite 配置（回退链）解析，两层都没有
/// 可用选择时降级为占位模型（`model_configured == false`，不阻断启动，见
/// [`crate::model::select_startup_model`]）；其余设置项按 CLI 参数 > 环境变量 >
/// sqlite 设置 > 协议默认 的优先级解析（ADR-0039）。
/// `policy` 决定是否在启动时创建/恢复 session（web 模式只开库不建 session）。
#[allow(clippy::too_many_lines)]
pub async fn bootstrap(cli: &Cli, policy: SessionPolicy) -> Result<AppState> {
    tracing::info!(?policy, "bootstrap: starting initialization");
    let env_openai_base_url = std::env::var("OPENAI_BASE_URL").ok();
    // session 库提前打开：设置快照、模型选择（config 表）与消息持久化共用同一库
    let store = open_store(cli).await?;
    tracing::debug!(
        store_available = store.is_some(),
        "bootstrap: session store opened"
    );
    // 设置快照（providers / model_specs / 标量，替代 config.toml）
    let settings = Settings::load(store.as_ref()).await;
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
        load_catalog_unless_complete(&settings, provider_hint.as_deref(), model_id_hint).await;
    let models = ModelResolver::new(cli, settings, env_openai_base_url, catalog);
    let startup = select_startup_model(cli, &db_history, &models)?;
    let model_configured = startup.configured;
    let model = startup.model;
    tracing::info!(
        model = %model.id,
        provider = %model.provider,
        api = ?model.api,
        configured = model_configured,
        "bootstrap: model selected"
    );
    let snapshot = models.settings();
    // api_key 显式分层解析（provider 内部的 env 回退发生在请求时，
    // 若把设置值直接交给构造器会抢到环境变量前面）。
    let api_key = resolve_api_key(
        cli.api_key.as_deref(),
        std::env::var(api_key_env(model.api)).ok().as_deref(),
        models
            .provider_row(&model.provider)
            .and_then(|p| p.api_key)
            .as_deref(),
    );
    let provider = crate::model::provider_for(&model, api_key.clone());
    // 思考级别恢复链：CLI > sqlite 配置表
    let db_reasoning = db_reasoning_level(store.as_ref()).await;
    let reasoning = cli
        .reasoning
        .as_deref()
        .map(parse_reasoning)
        .transpose()?
        .or(db_reasoning);
    let stream_options = StreamOptions {
        temperature: cli.temperature,
        max_tokens: cli.max_tokens,
        reasoning,
        api_key,
        headers: Vec::new(),
        timeout_ms: None,
    };
    let append_system = cli
        .append_system
        .as_deref()
        .or(snapshot.append_system.as_deref());
    let cwd = std::env::current_dir().context("get cwd")?;
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
    // 提示词配方（project 无关部分）；AGENTS.md 发现与 cwd 脚注在
    // session 的 project 确定后以其为基准构建
    let prompt_recipe = SystemPromptRecipe {
        append_system: append_system.map(str::to_string),
        active_skills,
    };
    let prompt_templates = load_prompt_templates(cli, &cwd, &snapshot.prompts)?;
    let session = match policy {
        SessionPolicy::Init => init_session(cli, &cwd, store.clone()).await?,
        SessionPolicy::OpenStoreOnly => None,
    };
    let project = session
        .as_ref()
        .map_or_else(|| normalize_path(&cwd), |init| init.project.clone());
    // project 首次初始化：惰性生成默认 nix 环境定义（仅 nix 可用时；
    // ADR-0041）。失败不阻断启动，bash 会回退宿主环境
    if let Err(error) = nomic_tools::nix_env::ensure_default_flake(&project) {
        tracing::warn!(%error, "创建默认 nix 环境定义失败");
    }
    // AGENTS.md 与 cwd 脚注以 session 的 project 为基准（project 严格
    // 归属，与工具基准同口径）：--session 跨目录恢复时提示词跟随目标
    // project 而非进程 cwd
    let system_prompt = prompt_recipe.build(&project, &skill_resolver);
    tracing::debug!(
        session_id = session.as_ref().map_or("none", |s| s.id.as_str()),
        history = session.as_ref().map_or(0, |s| s.history.len()),
        "bootstrap: session initialized"
    );
    let history = session
        .as_ref()
        .map(|init| init.history.clone())
        .unwrap_or_default();
    let compaction = snapshot.compaction.settings();
    // 所有可用模型列表（子 agent 模型选择用）
    let current_selection = crate::model::ModelSelection {
        provider: model.provider.clone(),
        model: model.id.clone(),
    };
    let available_models = models.all_models(&current_selection);
    // 模型别名（子 agent 模型选择用）：目标模型与主模型同一分层解析口径；
    // 配置指向未知 provider / 非法 spec 时硬报错（与配置文件校验同一口径）
    let model_aliases = resolve_model_aliases(&models)?;
    tracing::info!(
        model = %model.id,
        provider = %model.provider,
        skills = skill_resolver.catalog_with_diagnostics().skills.len(),
        prompt_templates = prompt_templates.len(),
        available_models = available_models.len(),
        "bootstrap: initialization complete"
    );
    // 全部组件注册为 state 服务（ADR-0047）：各入口与内部组件经
    // AppState 访问器提取依赖，消息总线随 state 传递到 serve 运行时
    Ok(AppState::new(Services {
        model,
        model_configured,
        models: Arc::new(models),
        provider,
        stream_options,
        system_prompt,
        compaction,
        store,
        session: session
            .as_ref()
            .map(|init| (init.store.clone(), init.id.clone())),
        project,
        history,
        prompt_recipe,
        skill_resolver,
        prompt_templates,
        available_models,
        model_aliases,
        bus: EventBus::new(),
    }))
}

/// 把 settings 表 `model_aliases` 的别名表解析为完整 [`Model`]：目标为
/// `<provider>/<模型id>` 全形式（写入时已校验格式），经 [`ModelResolver`]
/// 按与主模型相同的分层口径解析。
fn resolve_model_aliases(
    models: &ModelResolver,
) -> Result<std::collections::BTreeMap<String, Model>> {
    let mut resolved = std::collections::BTreeMap::new();
    for (alias, spec) in &models.settings().model_aliases {
        let selection = crate::model::ModelSelection::parse(spec, None)
            .with_context(|| format!("模型别名 {alias:?} 的目标 {spec:?} 非法"))?;
        let model = models
            .resolve(&selection.provider, &selection.model)
            .with_context(|| {
                format!(
                    "模型别名 {alias:?} 指向的模型 {} 无法解析",
                    selection.spec()
                )
            })?;
        resolved.insert(alias.clone(), model);
    }
    Ok(resolved)
}

/// 启动时把 skill 加载诊断对用户可见：坏 skill 被静默跳过会让人无从排查。
fn warn_skill_diagnostics(skill_resolver: &SkillResolver) {
    let catalog = skill_resolver.catalog_with_diagnostics();
    for error in &catalog.errors {
        tracing::warn!(error = ?error, "skipping failed skill: {error}");
    }
}

/// 加载 prompt templates：目录发现（`--no-prompt-templates` 关闭）+ 设置
/// `prompts` 与 `--prompt-template` 的显式路径（同名时优先级最高）。
/// 单个模板加载失败只告警不中断（与 skills 同一口径）。
fn load_prompt_templates(
    cli: &Cli,
    cwd: &Path,
    settings_prompts: &[PathBuf],
) -> Result<Vec<PromptTemplate>> {
    let mut explicit = settings_prompts.to_vec();
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
        tracing::warn!(error = ?error, "skipping failed prompt template");
    }
    Ok(catalog.templates)
}

/// session 初始化结果：store、session id 与恢复的历史消息（新会话为空）。
#[derive(Debug)]
struct SessionInit {
    store: SessionStore,
    id: String,
    history: Vec<Message>,
    /// session 所属 project 的规范化路径（本 session 所有操作的基准）
    project: PathBuf,
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
            tracing::warn!(error = ?error, "failed to open session store, persistence disabled for this run: {error}");
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
        let project = store.session_project_path(&id).await?;
        return Ok(Some(SessionInit {
            store,
            id,
            history,
            project,
        }));
    }
    if let Some(id) = &cli.session {
        // 显式 --session 尊重用户意图，可跨目录恢复，但跨目录时提示
        warn_if_cross_cwd(&store, id, cwd).await;
        let history = load_history(&store, id).await?;
        let project = store.session_project_path(id).await?;
        return Ok(Some(SessionInit {
            store,
            id: id.clone(),
            history,
            project,
        }));
    }

    match store.create_session(cwd).await {
        Ok(id) => Ok(Some(SessionInit {
            project: store.session_project_path(&id).await?,
            store,
            id,
            history: Vec::new(),
        })),
        Err(error) => {
            tracing::warn!(error = ?error, "failed to create session, persistence disabled for this run: {error}");
            Ok(None)
        }
    }
}

/// 当前规范化 cwd 下最近活跃的 work 的主 session id（work 是一等入口，
/// `--continue` 恢复其主 session）。
async fn latest_session_in(store: &SessionStore, cwd: &Path) -> Result<String> {
    let target = normalize_path(cwd);
    let works = store.list_works().await.context("列出 work 失败")?;
    works
        .into_iter()
        .find(|summary| normalize_path(&summary.project) == target)
        .map(|summary| summary.main_session_id)
        .with_context(|| {
            format!(
                "当前目录 {} 没有可恢复的 work\
                 （用 `nomic resume` 交互选择任意目录的 work）",
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
        && normalize_path(&summary.project) != normalize_path(cwd)
    {
        tracing::warn!(
            "session belongs to {}, different from current cwd",
            summary.project.display()
        );
    }
}

/// 路径规范化：优先 canonicalize（解析符号链接），路径不存在时退回原始路径。
fn normalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn parse_reasoning(level: &str) -> Result<ThinkingLevel> {
    level.parse().context("--reasoning 取值非法")
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
        assert_eq!(
            init.project,
            normalize_path(dir_b.path()),
            "--continue 恢复的 session 以所属 project 为操作基准"
        );
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
        assert!(message.contains("没有可恢复的 work"), "{message}");
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
        assert_eq!(
            init.project,
            normalize_path(dir_a.path()),
            "跨目录恢复：操作基准是 session 的 project 而非进程 cwd"
        );
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
