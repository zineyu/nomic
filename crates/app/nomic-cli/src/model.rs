//! provider/model 解析：启动路径与 TUI `/models` 运行时切换共用同一分层口径。
//!
//! provider/model 的选择按 CLI 参数 > sqlite 配置（config 表回退链）解析；
//! 两层都没有可用选择时降级为占位模型（[`unconfigured_model`]）：TUI / web
//! 照常启动，发消息时由 [`UnconfiguredProvider`] 返回引导错误，用户经
//! `models:<provider>/<模型id>` 或设置页在运行时完成选择（无内置默认模型）；
//! print 模式非交互，仍在启动时报错。base_url / api_key 等连接参数按
//! CLI 参数 > 环境变量 > providers 表 > settings 标量 > 协议默认 解析
//! （永远来自用户指定，ADR-0039）；模型字段（展示名、推理能力、上下文/
//! 输出上限、费率）逐字段按 model_specs 表 > models.dev > 中性兜底 解析。

use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use nomic_ai::{
    ApiKind, AssistantMessage, Catalog, Model, ModelSpec, Provider, StopReason, ThinkingLevel,
    providers::{AnthropicProvider, KimiProvider, OpenAiCompat, OpenAiProvider},
};
use nomic_session::{ProviderRow, SessionStore};

use crate::Cli;
use crate::settings::Settings;

/// `kimi_completions` API 的默认 base_url（Kimi For Coding 订阅端点）。
const KIMI_DEFAULT_BASE_URL: &str = "https://api.kimi.com/coding/v1";

/// `kimi_completions` API 在 models.dev 目录中的 provider id（规格查询别名：
/// 与 provider 名无关，凡 api 为 kimi_completions 的 provider 都命中该目录）。
const KIMI_CATALOG_ID: &str = "kimi-for-coding";

/// provider 各 API 家族对应的环境变量名（`api_key` 分层解析用）。
pub const fn api_key_env(api: ApiKind) -> &'static str {
    match api {
        ApiKind::AnthropicMessages => "ANTHROPIC_API_KEY",
        ApiKind::OpenAiCompletions => "OPENAI_API_KEY",
        ApiKind::KimiCompletions => "KIMI_API_KEY",
    }
}

/// models.dev 目录查询用的 provider id：`kimi_completions` API 别名到目录中的
/// `kimi-for-coding`（Moonshot 系模型都登记在该 id 下）；其余按 provider 名查询。
const fn catalog_provider_id(provider: &str, api: ApiKind) -> &str {
    match api {
        ApiKind::KimiCompletions => KIMI_CATALOG_ID,
        _ => provider,
    }
}

/// 按 API 种类构造 provider 连接实现（启动与 `/models` 运行时切换共用）。
///
/// `kimi_completions` 用 [`KimiProvider`]（OpenAI Completions 传输 + 固定
/// MFJS 工具 schema 方言）；`open_ai_completions` 及其他兼容端点用
/// [`OpenAiProvider`] 默认配置。
pub fn build_provider(api: ApiKind, api_key: Option<String>) -> Arc<dyn Provider> {
    match api {
        ApiKind::AnthropicMessages => Arc::new(AnthropicProvider::new(api_key)),
        ApiKind::KimiCompletions => Arc::new(KimiProvider::new(api_key)),
        ApiKind::OpenAiCompletions => {
            Arc::new(OpenAiProvider::new(api_key, OpenAiCompat::default()))
        }
    }
}
/// 占位模型的 provider 名 / 模型 id（[`unconfigured_model`]；与真实定义不
/// 冲突——providers 表写入校验将该名保留）。
pub const UNCONFIGURED: &str = "unconfigured";

/// 未配置模型时的引导文案：占位 provider 的流错误与 TUI 启动提示共用同一口径。
pub const UNCONFIGURED_GUIDANCE: &str = "尚未配置模型：发送消息前请先选择模型\
（TUI 用 models:<provider>/<模型id>，如 models:anthropic/claude-sonnet-4-5；\
provider 由 `nomic config providers` 定义，anthropic / openai 可按名推断）。";

/// print 模式无可用模型时的启动报错（非交互，无法在运行时选择，保持快速失败）。
pub const NO_MODEL_ERROR: &str = "未指定模型：请用 --model <provider>/<模型id> 指定";

/// 占位模型：CLI 与 sqlite 都没有可用模型选择时顶替启动（无内置默认模型，
/// 仅承载「未配置」状态；spec 字段全为中性兜底值，context_window 0 = 未知）。
/// 永不发起真实请求——配套 provider 是 [`UnconfiguredProvider`]。
pub fn unconfigured_model() -> Model {
    Model {
        name: "未配置模型".to_string(),
        id: UNCONFIGURED.to_string(),
        api: ApiKind::OpenAiCompletions,
        provider: UNCONFIGURED.to_string(),
        base_url: String::new(),
        reasoning: false,
        vision: false,
        context_window: 0,
        max_tokens: 0,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    }
}

/// 占位模型的配套 provider：不发起任何请求，`stream` 立即以
/// [`UNCONFIGURED_GUIDANCE`] 为错误信息的 `Error` 终止事件收尾（错误编码进流
/// 的 provider 契约），用户经 `models` 切换为真实模型后即恢复可用。
pub struct UnconfiguredProvider;

impl Provider for UnconfiguredProvider {
    fn stream(
        &self,
        model: &Model,
        _context: &nomic_ai::Context,
        _options: &nomic_ai::StreamOptions,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> nomic_ai::AssistantStream {
        let (tx, stream) = nomic_ai::channel();
        let message = AssistantMessage {
            content: Vec::new(),
            api: model.api,
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            usage: nomic_ai::Usage::default(),
            stop_reason: StopReason::Error,
            error_message: Some(UNCONFIGURED_GUIDANCE.to_string()),
            timestamp: nomic_ai::now_millis(),
        };
        let _ = tx.send(nomic_ai::AssistantEvent::Error {
            message: Box::new(message),
        });
        stream
    }
}

/// 按模型构造 provider 连接：占位模型配 [`UnconfiguredProvider`]（不请求），
/// 其余按 API 种类构造真实连接（启动与 web 按 session 构建共用）。
pub fn provider_for(model: &Model, api_key: Option<String>) -> Arc<dyn Provider> {
    if model.provider == UNCONFIGURED {
        Arc::new(UnconfiguredProvider)
    } else {
        build_provider(model.api, api_key)
    }
}

/// `--model` spec 中的 provider 段（`<provider>/<模型id>` 全形式时按第一个
/// `/` 切分，与 [`ModelSelection::parse`] 一致；无 `/` 时为 `None`）。
pub fn cli_model_provider(cli: &Cli) -> Option<String> {
    cli.model.as_deref().and_then(|spec| {
        spec.split_once('/')
            .map(|(provider, _)| provider.to_string())
    })
}

/// sqlite 配置表中模型选择的配置键。
pub const CONFIG_KEY_MODEL: &str = "model";

/// sqlite 配置表中思考级别的配置键。
pub const CONFIG_KEY_REASONING: &str = "reasoning";

/// 模型选择项：`<provider>/<模型id>` 格式（sqlite 配置与 `/models` 命令共用）。
/// 派生 serde（web 模式经 REST 返回当前选择）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelSelection {
    /// provider 名（anthropic / openai，或配置表定义的自定义名）
    pub provider: String,
    /// 模型 id
    pub model: String,
}

impl ModelSelection {
    /// 解析 `<provider>/<模型id>`：按第一个 `/` 切分（模型 id 自身可含 `/`，
    /// 如 openrouter 的 `openai/gpt-4o` 写作 `openrouter/openai/gpt-4o`）；
    /// 无 `/` 时用 `default_provider`，二者都缺时报错。
    pub fn parse(spec: &str, default_provider: Option<&str>) -> Result<Self> {
        match spec.split_once('/') {
            Some((provider, model)) if !provider.is_empty() && !model.is_empty() => Ok(Self {
                provider: provider.to_string(),
                model: model.to_string(),
            }),
            None if !spec.is_empty() => {
                let provider = default_provider.with_context(|| {
                    format!("模型选择项 {spec:?} 缺 provider：应为 <provider>/<模型id> 格式")
                })?;
                Ok(Self {
                    provider: provider.to_string(),
                    model: spec.to_string(),
                })
            }
            _ => bail!("模型选择项 {spec:?} 非法：应为 <provider>/<模型id> 格式"),
        }
    }

    /// `<provider>/<模型id>` 全形式（落库与展示用）。
    pub fn spec(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}

/// 数据库中的模型选择历史（最新在前的回退链）。
///
/// 库不可用或读取失败返回空链（告警后由调用方报错要求显式指定，不阻断启动）；
/// 形态非法的行跳过——回退语义要求一行损坏不阻断更早的可用选择。
pub async fn db_model_history(store: Option<&SessionStore>) -> Vec<ModelSelection> {
    let Some(store) = store else {
        return Vec::new();
    };
    let values = match store.config_history(CONFIG_KEY_MODEL).await {
        Ok(values) => values,
        Err(error) => {
            tracing::warn!(error = ?error, "failed to read model selection config: {error}");
            return Vec::new();
        }
    };
    values
        .iter()
        .filter_map(|value| {
            let parsed = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("配置值不是字符串"))
                .and_then(|spec| ModelSelection::parse(spec, None));
            match parsed {
                Ok(selection) => Some(selection),
                Err(error) => {
                    tracing::warn!(
                        value = %value,
                        error = ?error,
                        "skipping invalid model selection config ({error:#}), falling back to earlier selection"
                    );
                    None
                }
            }
        })
        .collect()
}

/// 从 sqlite 配置表读取上次保存的思考级别。
///
/// 库不可用或读取失败返回 `None`（降级为 CLI 默认）。
pub async fn db_reasoning_level(store: Option<&SessionStore>) -> Option<ThinkingLevel> {
    let store = store?;
    let value = store
        .config_history(CONFIG_KEY_REASONING)
        .await
        .ok()?
        .into_iter()
        .next()?;
    let word = value.as_str()?;
    word.parse().ok()
}

/// 启动模型选择的结果：解析出的模型 + 是否为真实配置。
///
/// `configured == false` 时 `model` 是占位模型（[`unconfigured_model`]）：
/// CLI 与 sqlite 都没有可用选择，TUI / web 照常启动、发消息时才报引导错误。
#[derive(Debug)]
pub struct StartupModel {
    /// 解析出的模型（未配置时为占位模型）
    pub model: Model,
    /// 是否来自真实配置（CLI 参数或 sqlite 选择历史）
    pub configured: bool,
}

/// 启动模型选择：CLI 参数 > sqlite 配置回退链（feedback）；两层都没有可用
/// 选择时降级为占位模型（`configured == false`），不阻断启动。
///
/// - `--model` 支持 `<provider>/<模型id>` 全形式（provider 部分优先于
///   `--provider`）；CLI 给出任一选择器时数据库选择整层不生效
/// - 无 CLI 选择器时沿数据库回退链从最新向最老逐条解析，第一条可解析的
///   选择生效（provider 已删除、模型已不存在的失效选择告警后回退）
/// - 链空或全部失效时返回占位模型：无内置默认模型，用户运行时经
///   `models:<provider>/<模型id>` 完成选择；print 模式由调用方转为报错
/// - CLI 显式指定但解析失败（未知 provider / 模型）仍硬报错：显式输入
///   有误应立刻暴露，不静默降级
pub fn select_startup_model(
    cli: &Cli,
    db_history: &[ModelSelection],
    models: &ModelResolver,
) -> Result<StartupModel> {
    if cli.provider.is_some() || cli.model.is_some() {
        let provider = cli_model_provider(cli)
            .or_else(|| cli.provider.clone())
            .context("--model 缺 provider：请用 <provider>/<模型id> 全形式，或搭配 --provider")?;
        let model_id = match &cli.model {
            Some(spec) => ModelSelection::parse(spec, Some(&provider))?.model,
            None => bail!("provider {provider:?} 无默认模型，请用 --model 指定模型 id"),
        };
        tracing::debug!(provider = %provider, model = %model_id, "selecting model from CLI");
        return Ok(StartupModel {
            model: models.resolve(&provider, &model_id)?,
            configured: true,
        });
    }
    for selection in db_history {
        match models.resolve(&selection.provider, &selection.model) {
            Ok(model) => {
                tracing::debug!(provider = %selection.provider, model = %selection.model, "model selected from db history");
                return Ok(StartupModel {
                    model,
                    configured: true,
                });
            }
            Err(error) => {
                tracing::warn!(
                    selection = %selection.spec(),
                    error = ?error,
                    "model selection {} is stale ({error:#}), falling back to earlier selection",
                    selection.spec()
                );
            }
        }
    }
    tracing::warn!(
        "no model configured (CLI and sqlite history both empty or stale), \
         starting with placeholder model"
    );
    Ok(StartupModel {
        model: unconfigured_model(),
        configured: false,
    })
}

/// 解析 api_key：CLI 参数 > 环境变量 > `providers.<名字>.api_key`。
pub fn resolve_api_key(
    cli: Option<&str>,
    env: Option<&str>,
    provider: Option<&str>,
) -> Option<String> {
    let source = if cli.is_some() {
        "cli"
    } else if env.is_some() {
        "env"
    } else if provider.is_some() {
        "provider_config"
    } else {
        "none"
    };
    tracing::debug!(
        source,
        has_key = cli.or(env).or(provider).is_some(),
        "api_key resolved"
    );
    cli.or(env).or(provider).map(str::to_string)
}

/// 取设置快照中 (provider, 模型id) 的规格覆盖行。
fn model_spec_override(
    settings: &Settings,
    provider_kind: &str,
    model_id: Option<&str>,
) -> Option<ModelSpec> {
    model_id.and_then(|id| {
        settings
            .model_specs
            .get(&(provider_kind.to_string(), id.to_string()))
            .cloned()
    })
}

/// 加载 models.dev 目录；设置已给全规格字段时跳过（不读缓存、不联网），
/// 目录不可用时告警并返回 `None`（调用方落到中性兜底）。
pub async fn load_catalog_unless_complete(
    settings: &Settings,
    provider_kind: Option<&str>,
    model_id_hint: Option<&str>,
) -> Option<Catalog> {
    let complete = provider_kind.is_some_and(|provider| {
        model_spec_override(settings, provider, model_id_hint)
            .is_some_and(|spec| spec.is_complete())
    });
    if complete {
        tracing::debug!("models.dev catalog skipped (settings have complete spec)");
        return None;
    }
    let catalog = nomic_ai::models_dev::load().await;
    if catalog.is_none() {
        tracing::warn!(
            "models.dev catalog unavailable, falling back to neutral model spec defaults"
        );
    }
    catalog
}

/// 分层解析的最底层：协议级默认 base URL 与保守的规格兜底值（全零）。
struct Preset {
    /// 默认 base URL（内置 provider 有专属地址，其余按 API 协议）
    default_base_url: &'static str,
    /// 规格兜底值（除 `name` 外全字段有值；`name` 缺省回退为模型 id）
    spec: ModelSpec,
}

/// 中性兜底：规格字段全为保守值，base URL 按 API 协议取官方地址
/// （`kimi_completions` 指向 Kimi For Coding 订阅端点）。
const fn neutral_preset(api: ApiKind) -> Preset {
    let default_base_url = match api {
        ApiKind::AnthropicMessages => "https://api.anthropic.com",
        ApiKind::OpenAiCompletions => "https://api.openai.com/v1",
        ApiKind::KimiCompletions => KIMI_DEFAULT_BASE_URL,
    };
    Preset {
        default_base_url,
        spec: ModelSpec {
            name: None,
            reasoning: Some(false),
            vision: Some(false),
            context_window: Some(0),
            max_tokens: Some(0),
            cost_input: Some(0.0),
            cost_output: Some(0.0),
            cost_cache_read: Some(0.0),
            cost_cache_write: Some(0.0),
        },
    }
}

/// `/models` 选择器的一行候选：`<provider, 模型id>` + 解析后的展示信息。
/// 派生 serde（web 模式经 REST 列表给前端模型选择器）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelChoice {
    /// provider 名
    pub provider: String,
    /// 模型 id
    pub id: String,
    /// 展示名（规格缺省时回退为模型 id）
    pub name: String,
    /// 上下文窗口 token 数（0 = 规格未知）
    pub context_window: u64,
    /// 是否支持推理/思考（选择器标注用）
    pub reasoning: bool,
}

impl ModelChoice {
    /// `<provider>/<模型id>` 选择项（切换时回传给 [`ModelResolver::resolve`] 与落库）。
    pub fn spec(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// 运行时模型解析器：持有全部 provider 的连接层输入（CLI 覆盖、环境变量、
/// sqlite 设置快照、models.dev 目录），按 `<provider, 模型id>` 重复解析完整
/// [`Model`]。启动解析与 TUI `/models` 运行时切换共用同一分层口径。
///
/// 设置快照（ADR-0039）放在 `RwLock` 里：`nomic config` / TUI `/config` /
/// web 设置事件写入落库后经 [`Self::reload`] 刷新，运行进程立即生效。
pub struct ModelResolver {
    settings: std::sync::RwLock<Settings>,
    catalog: Option<Catalog>,
    cli_base_url: Option<String>,
    env_openai_base_url: Option<String>,
}

impl ModelResolver {
    /// 捕获启动时的解析输入（`cli` 中只有 `--base-url` 参与模型解析）。
    pub fn new(
        cli: &Cli,
        settings: Settings,
        env_openai_base_url: Option<String>,
        catalog: Option<Catalog>,
    ) -> Self {
        Self {
            settings: std::sync::RwLock::new(settings),
            catalog,
            cli_base_url: cli.base_url.clone(),
            env_openai_base_url,
        }
    }

    /// 设置快照（克隆读出；解析均为冷路径，克隆成本可忽略）。
    ///
    /// # Panics
    ///
    /// 设置锁中毒时 panic（锁内无 panic 路径，正常不会触发）。
    pub fn settings(&self) -> Settings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    /// 从 store 重新加载设置快照（设置写入落库后调用，运行进程立即生效）。
    ///
    /// # Panics
    ///
    /// 设置锁中毒时 panic（锁内无 panic 路径，正常不会触发）。
    /// 从 store 重新加载设置快照（设置写入落库后调用，运行进程立即生效）。
    ///
    /// # Panics
    ///
    /// 设置锁中毒时 panic（锁内无 panic 路径，正常不会触发）。
    pub async fn reload(&self, store: Option<&SessionStore>) {
        let settings = Settings::load(store).await;
        *self.settings.write().expect("settings lock poisoned") = settings;
    }

    /// 指定 provider 的定义行（providers 表）。
    ///
    /// # Panics
    ///
    /// 设置锁中毒时 panic（锁内无 panic 路径，正常不会触发）。
    pub fn provider_row(&self, provider: &str) -> Option<ProviderRow> {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .providers
            .get(provider)
            .cloned()
    }

    /// provider 的 API 种类：providers 表显式设置 > 按名推断
    /// （anthropic / openai）；其余名字必须在 providers 表中定义并给出 api。
    fn api(&self, provider: &str) -> Result<ApiKind> {
        self.provider_row(provider)
            .and_then(|p| p.api)
            .or_else(|| crate::settings::infer_api(provider))
            .with_context(|| {
                format!(
                    "未知 provider {provider:?}：请用 `nomic config providers set {provider} \
                     --api <anthropic_messages|open_ai_completions> --base-url <url>` 定义\
                     （anthropic / openai 可按名自动推断）"
                )
            })
    }

    /// base_url 永远来自用户指定：CLI > 环境变量（仅 openai 系）>
    /// providers 表 > 协议默认地址，不经由 models.dev。
    fn base_url(&self, provider: &str, api: ApiKind, preset: &Preset) -> String {
        let settings = self.settings();
        self.cli_base_url
            .clone()
            .or_else(|| {
                self.env_openai_base_url
                    .as_deref()
                    .filter(|_| api == ApiKind::OpenAiCompletions)
                    .map(str::to_string)
            })
            .or_else(|| {
                settings
                    .providers
                    .get(provider)
                    .and_then(|p| p.base_url.clone())
            })
            .unwrap_or_else(|| preset.default_base_url.to_string())
    }

    /// 规格字段（`name` / `reasoning` / `vision` / `context_window` / `max_tokens` /
    /// `cost_*`）逐字段分层：model_specs 表 > models.dev > 中性兜底。
    fn spec_for(&self, provider: &str, model_id: &str, api: ApiKind, preset: &Preset) -> ModelSpec {
        model_spec_override(&self.settings(), provider, Some(model_id))
            .unwrap_or_default()
            .or_fill(
                &self
                    .catalog
                    .as_ref()
                    .and_then(|c| c.lookup(Some(catalog_provider_id(provider, api)), model_id))
                    .cloned()
                    .unwrap_or_default(),
            )
            .or_fill(&preset.spec)
    }

    /// 按 `<provider, 模型id>` 解析完整 [`Model`]（分层与启动时一致）。
    ///
    /// 校验模型存在（禁止配置不存在的模型）：配置覆盖表 / models.dev 目录必须
    /// 命中其一，否则报错——启动路径直接失败，`/models` 运行时切换转为提示。
    /// 目录不可用（离线）时无法校验，保持既有降级行为。
    pub fn resolve(&self, provider: &str, model_id: &str) -> Result<Model> {
        let api = self.api(provider)?;
        let preset = neutral_preset(api);
        self.ensure_known(provider, model_id, api)?;
        let base_url = self.base_url(provider, api, &preset);
        let spec = self.spec_for(provider, model_id, api, &preset);
        tracing::debug!(
            provider = %provider,
            model = %model_id,
            api = ?api,
            base_url = %base_url,
            reasoning = spec.reasoning.unwrap_or(false),
            "model resolved"
        );
        Ok(Model {
            name: spec.name.unwrap_or_else(|| model_id.to_string()),
            id: model_id.to_string(),
            api,
            provider: provider.to_string(),
            base_url,
            reasoning: spec.reasoning.unwrap_or(false),
            vision: spec.vision.unwrap_or(false),
            context_window: spec.context_window.unwrap_or(0),
            max_tokens: spec.max_tokens.unwrap_or(0),
            cost_input: spec.cost_input.unwrap_or(0.0),
            cost_output: spec.cost_output.unwrap_or(0.0),
            cost_cache_read: spec.cost_cache_read.unwrap_or(0.0),
            cost_cache_write: spec.cost_cache_write.unwrap_or(0.0),
        })
    }

    /// 模型存在性校验：model_specs 覆盖表（用户显式定义）→ models.dev 目录。
    ///
    /// 目录不可用（离线 / 设置已写全规格跳过加载）时不校验——没有权威数据源
    /// 无法判断「不存在」，维持启动告警 + 中性兜底的降级语义。
    fn ensure_known(&self, provider: &str, model_id: &str, api: ApiKind) -> Result<()> {
        if model_spec_override(&self.settings(), provider, Some(model_id)).is_some()
            || self.catalog.as_ref().is_some_and(|catalog| {
                catalog
                    .lookup(Some(catalog_provider_id(provider, api)), model_id)
                    .is_some()
            })
        {
            return Ok(());
        }
        if self.catalog.is_none() {
            // 目录不可用：无法校验存在性，保持降级行为
            tracing::debug!(provider = %provider, model = %model_id, "model validation skipped (catalog unavailable)");
            return Ok(());
        }
        tracing::warn!(provider = %provider, model = %model_id, "model not found in catalog or settings");
        Err(anyhow::anyhow!(
            "模型 {model_id:?} 不存在：不在 models.dev 目录中，\
             也未在 model_specs 设置中定义，\
             请检查 model / --model 拼写，或用 `nomic config models set {provider}/{model_id} ...`\
             补充该模型的规格"
        ))
    }

    /// `/models` 选择器候选（跨 provider）：每个 provider 的 规格覆盖 ∪
    /// models.dev 目录 ∪ 当前模型；provider 间按名排序（当前模型所在的
    /// provider 未在 providers 表中定义时补入，保证当前模型始终可见）、
    /// provider 内按模型 id 排序去重。
    ///
    /// 目录不可用（启动时已告警）或 provider 名不命中 models.dev 时，该
    /// provider 只剩规格覆盖与当前模型；`/models:<p>/<id>` 直接切换不受候选
    /// 范围限制。api 解析失败的 provider 整组跳过。
    ///
    /// # Panics
    ///
    /// 设置锁中毒时 panic（锁内无 panic 路径，正常不会触发）。
    pub fn candidates(&self, current: &ModelSelection) -> Vec<ModelChoice> {
        let settings = self.settings();
        let mut choices = Vec::new();
        let mut providers: Vec<String> = settings.providers.keys().cloned().collect();
        if !providers.contains(&current.provider) {
            providers.insert(0, current.provider.clone());
        }
        for provider in providers {
            let Ok(api) = self.api(&provider) else {
                continue;
            };
            let preset = neutral_preset(api);
            let mut ids = std::collections::BTreeSet::new();
            if provider == current.provider {
                ids.insert(current.model.clone());
            }
            ids.extend(
                settings
                    .model_specs
                    .keys()
                    .filter(|(p, _)| p == &provider)
                    .map(|(_, id)| id.clone()),
            );
            if let Some(catalog) = &self.catalog {
                ids.extend(
                    catalog
                        .models_of(catalog_provider_id(&provider, api))
                        .into_iter()
                        .map(|(id, _)| id.to_string()),
                );
            }
            choices.extend(ids.into_iter().map(|id| {
                let spec = self.spec_for(&provider, &id, api, &preset);
                let name = spec.name.unwrap_or_else(|| id.clone());
                ModelChoice {
                    provider: provider.clone(),
                    id,
                    name,
                    context_window: spec.context_window.unwrap_or(0),
                    reasoning: spec.reasoning.unwrap_or(false),
                }
            }));
        }
        choices
    }

    /// 所有可用模型的完整 [`Model`] 列表（子 agent 模型选择用）。
    ///
    /// 基于 [`Self::candidates`] 枚举每个候选，逐个经 [`Self::resolve`]
    /// 解析为完整 Model（含 base_url / cost 等字段）。解析失败的候选
    /// 跳过（告警），不阻断其余模型。
    pub fn all_models(&self, current: &ModelSelection) -> Vec<Model> {
        self.candidates(current)
            .into_iter()
            .filter_map(|choice| self.resolve(&choice.provider, &choice.id).ok())
            .collect()
    }
}

/// 解析启动模型（[`ModelResolver`] 的启动路径包装，仅测试使用）。
#[cfg(test)]
fn resolve_model(
    provider_kind: &str,
    cli: &Cli,
    settings: Settings,
    env_openai_base_url: Option<&str>,
    catalog: Option<&Catalog>,
) -> Result<Model> {
    let resolver = ModelResolver::new(
        cli,
        settings,
        env_openai_base_url.map(str::to_string),
        catalog.cloned(),
    );
    // 未知 provider 优先报「未知 provider」（保持原报错顺序）
    resolver.api(provider_kind)?;
    let model_id = match &cli.model {
        Some(spec) => ModelSelection::parse(spec, Some(provider_kind))?.model,
        None => bail!("provider {provider_kind:?} 无默认模型，请用 --model 指定模型 id"),
    };
    resolver.resolve(provider_kind, &model_id)
}

#[cfg(test)]
mod tests;
