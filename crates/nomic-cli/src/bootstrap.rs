//! 两种模式共享的运行时初始化：provider/model 解析、stream options、系统提示词、
//! session 新建/恢复。
//!
//! provider / base_url / api_key 等连接参数按 CLI 参数 > 环境变量 >
//! `providers.<名字>.*` > 平铺配置 > 内置默认 解析（永远来自用户指定）；
//! 模型规格字段（展示名、推理能力、上下文/输出上限、费率）逐字段按
//! 配置 `providers.<名字>.models.<模型id>` > models.dev > 内置预设 解析。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use nomic_ai::{
    ApiKind, Catalog, Message, Model, ModelSpec, Provider, StreamOptions, ThinkingLevel,
    providers::{AnthropicProvider, OpenAiCompat, OpenAiProvider},
};
use nomic_prompts::{ProjectDiscovery, PromptResolver, PromptTemplate};
use nomic_session::SessionStore;
use nomic_skills::{ActivatedSkill, SkillResolver};

use crate::Cli;
use crate::config::{Config, ProviderConfig};
use crate::context_files::{ContextFile, discover_agents_files};

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
    let models = ModelResolver::new(cli, config, env_openai_base_url, catalog);
    let model_id = models.default_model_id(cli, &provider_kind)?;
    let model = models.resolve(&provider_kind, &model_id)?;
    // api_key 显式分层解析（provider 内部的 env 回退发生在请求时，
    // 若把配置文件值直接交给构造器会抢到环境变量前面）。
    let api_key = resolve_api_key(
        cli.api_key.as_deref(),
        std::env::var(api_key_env(model.api)).ok().as_deref(),
        models
            .provider_config(&provider_kind)
            .and_then(|p| p.api_key.as_deref()),
        models.config().and_then(|c| c.api_key.as_deref()),
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
            .or_else(|| models.config().and_then(|c| c.temperature)),
        max_tokens: cli
            .max_tokens
            .or_else(|| models.config().and_then(|c| c.max_tokens)),
        reasoning: cli
            .reasoning
            .as_deref()
            .or_else(|| models.config().and_then(|c| c.reasoning.as_deref()))
            .map(parse_reasoning)
            .transpose()?,
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
    let session = init_session(cli, &cwd).await?;
    let history = session
        .as_ref()
        .map(|init| init.history.clone())
        .unwrap_or_default();
    let compaction = compaction_settings(models.config());
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
    })
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
) -> Option<&'c ProviderConfig> {
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

/// `/models` 选择器的一行候选：模型 id + 解析后的展示信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// 模型 id（切换时回传给 [`ModelResolver::resolve`]）
    pub id: String,
    /// 展示名（规格缺省时回退为模型 id）
    pub name: String,
    /// 上下文窗口 token 数（0 = 规格未知）
    pub context_window: u64,
    /// 是否支持推理/思考（选择器标注用）
    pub reasoning: bool,
}

/// 运行时模型解析器：持有全部 provider 的连接层输入（CLI 覆盖、环境变量、
/// 配置文件、models.dev 目录），按 `<provider, 模型id>` 重复解析完整 [`Model`]。
/// 启动解析与 TUI `/models` 运行时切换共用同一分层口径。
pub struct ModelResolver {
    config: Option<Config>,
    catalog: Option<Catalog>,
    cli_base_url: Option<String>,
    env_openai_base_url: Option<String>,
}

impl ModelResolver {
    /// 捕获启动时的解析输入（`cli` 中只有 `--base-url` 参与模型解析）。
    fn new(
        cli: &Cli,
        config: Option<Config>,
        env_openai_base_url: Option<String>,
        catalog: Option<Catalog>,
    ) -> Self {
        Self {
            config,
            catalog,
            cli_base_url: cli.base_url.clone(),
            env_openai_base_url,
        }
    }

    /// 配置文件层（`stream_options` 等其他分层仍需要）。
    const fn config(&self) -> Option<&Config> {
        self.config.as_ref()
    }

    /// 指定 provider 的配置表定义。
    pub fn provider_config(&self, provider: &str) -> Option<&ProviderConfig> {
        provider_config(self.config(), provider)
    }

    /// provider 的 API 种类：配置显式指定 > 内置名推断；未知 provider 报错。
    fn api(&self, provider: &str) -> Result<ApiKind> {
        self.provider_config(provider)
            .and_then(|p| p.api)
            .or_else(|| crate::config::infer_api(provider))
            .with_context(|| {
                format!(
                    "未知 provider {provider:?}：请使用 anthropic / openai，\
                     或在 config.toml 的 [providers.{provider}] 中定义并指定 api"
                )
            })
    }

    /// 分层最底层的预设（内置 provider 或自定义中性预设）。
    fn preset(provider: &str, api: ApiKind) -> Preset {
        builtin_preset(provider).unwrap_or_else(|| neutral_preset(api))
    }

    /// base_url 永远来自用户指定：CLI > 环境变量（仅 openai 系）>
    /// `providers.<名字>.*` > 平铺配置 > 内置默认，不经由 models.dev。
    fn base_url(&self, provider: &str, api: ApiKind, preset: &Preset) -> String {
        self.cli_base_url
            .clone()
            .or_else(|| {
                self.env_openai_base_url
                    .as_deref()
                    .filter(|_| api == ApiKind::OpenAiCompletions)
                    .map(str::to_string)
            })
            .or_else(|| {
                self.provider_config(provider)
                    .and_then(|p| p.base_url.clone())
            })
            .or_else(|| self.config().and_then(|c| c.base_url.clone()))
            .unwrap_or_else(|| preset.default_base_url.to_string())
    }

    /// 默认模型 id：CLI 参数 > 配置文件 > 内置默认（自定义 provider 无默认时报错）。
    fn default_model_id(&self, cli: &Cli, provider: &str) -> Result<String> {
        cli.model
            .clone()
            .or_else(|| self.config().and_then(|c| c.model.clone()))
            .or_else(|| {
                builtin_preset(provider).and_then(|preset| preset.default_model.map(str::to_string))
            })
            .with_context(|| {
                format!("provider {provider:?} 没有内置默认模型，请用 --model 或配置 model 指定")
            })
    }

    /// 规格字段（`name` / `reasoning` / `context_window` / `max_tokens` / `cost_*`）
    /// 逐字段分层：配置 `providers.<名字>.models.<模型id>` > models.dev > 内置预设。
    fn spec_for(&self, provider: &str, model_id: &str, preset: &Preset) -> ModelSpec {
        model_spec_from_config(self.config(), provider, Some(model_id))
            .cloned()
            .unwrap_or_default()
            .or_fill(
                &self
                    .catalog
                    .as_ref()
                    .and_then(|c| c.lookup(Some(provider), model_id))
                    .cloned()
                    .unwrap_or_default(),
            )
            .or_fill(&preset.spec)
    }

    /// 按 `<provider, 模型id>` 解析完整 [`Model`]（分层与启动时一致）。
    ///
    /// 校验模型存在（禁止配置不存在的模型）：配置覆盖表 / models.dev 目录 /
    /// 内置默认模型必须命中其一，否则报错——启动路径直接失败，`/models` 运行时
    /// 切换转为提示。目录不可用（离线）时无法校验，保持既有降级行为。
    pub fn resolve(&self, provider: &str, model_id: &str) -> Result<Model> {
        let api = self.api(provider)?;
        let preset = Self::preset(provider, api);
        self.ensure_known(provider, model_id, &preset)?;
        let base_url = self.base_url(provider, api, &preset);
        let spec = self.spec_for(provider, model_id, &preset);
        Ok(Model {
            name: spec.name.unwrap_or_else(|| model_id.to_string()),
            id: model_id.to_string(),
            api,
            provider: provider.to_string(),
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

    /// 模型存在性校验：配置覆盖表（用户显式定义）→ models.dev 目录 →
    /// 内置默认模型。
    ///
    /// 目录不可用（离线 / 配置已写全规格跳过加载）时不校验——没有权威数据源
    /// 无法判断「不存在」，维持启动告警 + 内置预设的降级语义。
    fn ensure_known(&self, provider: &str, model_id: &str, preset: &Preset) -> Result<()> {
        if model_spec_from_config(self.config(), provider, Some(model_id)).is_some()
            || self
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.lookup(Some(provider), model_id).is_some())
            || preset.default_model == Some(model_id)
        {
            return Ok(());
        }
        if self.catalog.is_none() {
            // 目录不可用：无法校验存在性，保持降级行为
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "模型 {model_id:?} 不存在：不在 models.dev 目录中，\
             也未在 config.toml 的 [providers.{provider}.models] 下定义，\
             请检查 model / --model 拼写，或在该表中补充该模型的规格"
        ))
    }

    /// `/models` 选择器候选：配置覆盖 ∪ models.dev 目录 ∪ 内置默认模型
    /// ∪ 当前模型，按 id 排序去重。
    ///
    /// 目录不可用（启动时已告警）或自定义 provider 名不命中 models.dev 时，
    /// 只剩配置覆盖、内置默认与当前模型；`/models:<id>` 直接切换不受候选
    /// 范围限制。api 解析失败理论不可达（启动已成功解析过），此时返回空列表。
    pub fn candidates(&self, provider: &str, current_id: &str) -> Vec<ModelChoice> {
        let Ok(api) = self.api(provider) else {
            return Vec::new();
        };
        let preset = Self::preset(provider, api);
        let mut ids = std::collections::BTreeSet::new();
        ids.insert(current_id.to_string());
        if let Some(models) = self
            .provider_config(provider)
            .and_then(|p| p.models.as_ref())
        {
            ids.extend(models.keys().cloned());
        }
        if let Some(catalog) = &self.catalog {
            ids.extend(
                catalog
                    .models_of(provider)
                    .into_iter()
                    .map(|(id, _)| id.to_string()),
            );
        }
        if let Some(preset) = builtin_preset(provider)
            && let Some(default) = preset.default_model
        {
            ids.insert(default.to_string());
        }
        ids.into_iter()
            .map(|id| {
                let spec = self.spec_for(provider, &id, &preset);
                let name = spec.name.unwrap_or_else(|| id.clone());
                ModelChoice {
                    id,
                    name,
                    context_window: spec.context_window.unwrap_or(0),
                    reasoning: spec.reasoning.unwrap_or(false),
                }
            })
            .collect()
    }
}

/// 解析启动模型（[`ModelResolver`] 的启动路径包装，仅测试使用）。
#[cfg(test)]
fn resolve_model(
    provider_kind: &str,
    cli: &Cli,
    config: Option<&Config>,
    env_openai_base_url: Option<&str>,
    catalog: Option<&Catalog>,
) -> Result<Model> {
    let resolver = ModelResolver::new(
        cli,
        config.cloned(),
        env_openai_base_url.map(str::to_string),
        catalog.cloned(),
    );
    // 未知 provider 优先报「未知 provider」（保持原报错顺序）
    resolver.api(provider_kind)?;
    resolver.resolve(
        provider_kind,
        &resolver.default_model_id(cli, provider_kind)?,
    )
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
        // CLI 指定的模型不在配置/models.dev 中时必须在配置里定义规格
        let mut models = std::collections::BTreeMap::new();
        models.insert("cli-model".to_string(), ModelSpec::default());
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                models: Some(models),
                ..ProviderConfig::default()
            },
        );
        let config = Config {
            model: Some("config-model".to_string()),
            providers: Some(providers),
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
            ProviderConfig {
                base_url: Some("https://provider-table".to_string()),
                ..ProviderConfig::default()
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
            ProviderConfig {
                models: Some(models),
                ..ProviderConfig::default()
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
            ProviderConfig {
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
        let mut config = deepseek_config();
        // 无 models.dev 目录时自定义模型必须在配置里定义规格（存在性校验）
        let mut models = std::collections::BTreeMap::new();
        models.insert("deepseek-chat".to_string(), ModelSpec::default());
        let providers = config.providers.as_mut().expect("providers");
        providers.get_mut("deepseek").expect("deepseek").models = Some(models);
        let model = resolve("deepseek", &cli(&[]), Some(&config), None, None);
        assert_eq!(model.name, "deepseek-chat");
        assert_eq!(model.context_window, 0);
        assert!(!model.reasoning);
        assert_eq!(Some(model.cost_input), Some(0.0));
    }

    #[test]
    fn cli_reasoning_rejects_invalid_levels() {
        assert_eq!(parse_reasoning("low").expect("low"), ThinkingLevel::Low);
        assert_eq!(
            parse_reasoning("medium").expect("medium"),
            ThinkingLevel::Medium
        );
        assert!(parse_reasoning("extreme").is_err(), "非法级别必须报错");
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

    // ── /models：运行时模型解析器 ─────────────────────────────────────────

    fn resolver(cli: &Cli, config: Option<Config>, catalog: Option<Catalog>) -> ModelResolver {
        ModelResolver::new(cli, config, None, catalog)
    }

    #[test]
    fn resolve_by_id_shares_startup_layering() {
        // 与启动解析同一口径：配置覆盖 > models.dev > 内置预设
        let resolver = resolver(&cli(&[]), None, Some(catalog()));
        let model = resolver.resolve("openai", "gpt-5.2").expect("resolve");
        assert_eq!(model.name, "GPT-5.2");
        assert_eq!(model.context_window, 400_000);
        assert_eq!(model.base_url, "https://api.openai.com/v1");
        // 目录外的未知模型 id：报错（禁止配置不存在的模型）
        let error = resolver
            .resolve("openai", "gpt-future")
            .expect_err("目录可用时未知模型必须报错");
        assert!(format!("{error:#}").contains("不存在"));
    }

    // ── 模型存在性校验（禁止配置不存在的模型） ──────────────────────────────

    #[test]
    fn unknown_model_rejected_when_catalog_available() {
        let error = resolve_model(
            "openai",
            &cli(&["--model", "gpt-future"]),
            None,
            None,
            Some(&catalog()),
        )
        .expect_err("目录可用时未知模型必须报错");
        let message = format!("{error:#}");
        assert!(message.contains("gpt-future"), "{message}");
        assert!(message.contains("不存在"), "{message}");
        assert!(message.contains("models"), "报错需提示配置表位置");
    }

    #[test]
    fn config_defined_model_is_known_without_catalog() {
        // 配置覆盖表显式定义即「存在」：即便目录不可用（None）也可解析
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "my-fine-tune".to_string(),
            ModelSpec {
                name: Some("My Fine Tune".to_string()),
                ..ModelSpec::default()
            },
        );
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                models: Some(models),
                ..ProviderConfig::default()
            },
        );
        let config = Config {
            providers: Some(providers),
            ..Config::default()
        };
        let model = resolve_model(
            "openai",
            &cli(&["--model", "my-fine-tune"]),
            Some(&config),
            None,
            None,
        )
        .expect("配置表定义的模型可解析");
        assert_eq!(model.name, "My Fine Tune");
        assert_eq!(model.context_window, 400_000, "规格字段仍按分层解析");
    }

    #[test]
    fn unknown_model_degraded_when_catalog_unavailable() {
        // 离线降级：目录不可用无法校验存在性，保持既有回落行为（启动告警已发）
        let model = resolve_model("openai", &cli(&["--model", "gpt-future"]), None, None, None)
            .expect("目录不可用时未知模型回落内置预设");
        assert_eq!(model.id, "gpt-future");
        assert_eq!(model.context_window, 400_000);
    }

    #[test]
    fn candidates_merge_config_catalog_and_current() {
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "my-fine-tune".to_string(),
            ModelSpec {
                name: Some("My Fine Tune".to_string()),
                ..ModelSpec::default()
            },
        );
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                models: Some(models),
                ..ProviderConfig::default()
            },
        );
        let config = Config {
            providers: Some(providers),
            ..Config::default()
        };
        let resolver = resolver(&cli(&[]), Some(config), Some(catalog()));
        let choices = resolver.candidates("openai", "gpt-future");
        let ids: Vec<&str> = choices.iter().map(|choice| choice.id.as_str()).collect();
        assert_eq!(
            ids,
            ["gpt-5.2", "gpt-future", "my-fine-tune"],
            "目录 ∪ 当前模型 ∪ 配置覆盖，按 id 排序去重"
        );
        assert_eq!(choices[0].name, "GPT-5.2");
        assert_eq!(choices[0].context_window, 400_000);
        assert!(choices[0].reasoning, "gpt-5.2 的推理能力来自 models.dev");
        assert_eq!(choices[2].name, "My Fine Tune", "展示名来自配置覆盖");
        // 重复解析当前模型：候选中再选一次幂等
        let again = resolver.candidates("openai", "gpt-5.2");
        assert_eq!(again.len(), 2, "gpt-future 不再是当前模型后不出现");
    }

    #[test]
    fn candidates_without_catalog_keep_config_and_current() {
        // 目录不可用（如配置已写全规格字段跳过加载）：仍列出配置覆盖与当前模型
        let config = deepseek_config();
        let resolver = resolver(&cli(&[]), Some(config), None);
        let choices = resolver.candidates("deepseek", "deepseek-chat");
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].id, "deepseek-chat");
        assert_eq!(choices[0].context_window, 0, "无目录时规格落中性预设");
        assert!(!choices[0].reasoning, "中性预设不支持推理");
    }
}
