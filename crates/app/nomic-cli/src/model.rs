//! provider/model 解析：启动路径与 TUI `/models` 运行时切换共用同一分层口径。
//!
//! provider/model 的选择按 CLI 参数 > sqlite 配置（config 表回退链）解析，
//! 两层都没有时启动报错（无内置默认模型）；base_url / api_key 等连接参数按
//! CLI 参数 > 环境变量 > `providers.<名字>.*` > 平铺配置 > 协议默认 解析
//! （永远来自用户指定）；模型规格字段（展示名、推理能力、上下文/输出上限、
//! 费率）逐字段按 配置 `providers.<名字>.models.<模型id>` > models.dev >
//! 中性兜底 解析。

use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use nomic_ai::{
    ApiKind, Catalog, Model, ModelSpec, Provider, ThinkingLevel,
    providers::{AnthropicProvider, OpenAiCompat, OpenAiProvider},
};
use nomic_session::SessionStore;

use crate::Cli;
use crate::config::{Config, ProviderConfig};

/// provider 各 API 家族对应的环境变量名（`api_key` 分层解析用）。
pub const fn api_key_env(api: ApiKind) -> &'static str {
    match api {
        ApiKind::AnthropicMessages => "ANTHROPIC_API_KEY",
        ApiKind::OpenAiCompletions => "OPENAI_API_KEY",
    }
}

/// 按 API 种类构造 provider 连接实现（启动与 `/models` 运行时切换共用）。
pub fn build_provider(api: ApiKind, api_key: Option<String>) -> Arc<dyn Provider> {
    match api {
        ApiKind::AnthropicMessages => Arc::new(AnthropicProvider::new(api_key)),
        ApiKind::OpenAiCompletions => {
            Arc::new(OpenAiProvider::new(api_key, OpenAiCompat::default()))
        }
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
            eprintln!("\x1b[33m⚠ 读取模型选择配置失败：{error}\x1b[0m");
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
                    eprintln!(
                        "\x1b[33m⚠ 跳过非法的模型选择配置（{error:#}），回退到更早的选择\x1b[0m"
                    );
                    None
                }
            }
        })
        .collect()
}

/// 从 sqlite 配置表读取上次保存的思考级别。
///
/// 库不可用或读取失败返回 `None`（降级为 config.toml / CLI 默认）。
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

/// 启动模型选择：CLI 参数 > sqlite 配置回退链（feedback）；两层都没有时报错。
///
/// - `--model` 支持 `<provider>/<模型id>` 全形式（provider 部分优先于
///   `--provider`）；CLI 给出任一选择器时数据库选择整层不生效
/// - 无 CLI 选择器时沿数据库回退链从最新向最老逐条解析，第一条可解析的
///   选择生效（provider 已删除、模型已不存在的失效选择告警后回退）
/// - 链空或全部失效时报错：无内置默认模型，必须显式指定
pub fn select_startup_model(
    cli: &Cli,
    db_history: &[ModelSelection],
    models: &ModelResolver,
) -> Result<Model> {
    if cli.provider.is_some() || cli.model.is_some() {
        let provider = cli_model_provider(cli)
            .or_else(|| cli.provider.clone())
            .context("--model 缺 provider：请用 <provider>/<模型id> 全形式，或搭配 --provider")?;
        let model_id = match &cli.model {
            Some(spec) => ModelSelection::parse(spec, Some(&provider))?.model,
            None => bail!("provider {provider:?} 无默认模型，请用 --model 指定模型 id"),
        };
        return models.resolve(&provider, &model_id);
    }
    for selection in db_history {
        match models.resolve(&selection.provider, &selection.model) {
            Ok(model) => return Ok(model),
            Err(error) => eprintln!(
                "\x1b[33m⚠ 模型选择 {} 已失效（{error:#}），回退到更早的选择\x1b[0m",
                selection.spec()
            ),
        }
    }
    bail!(
        "未指定模型：请用 --model <provider>/<模型id> 指定（provider 在 config.toml 的 [providers] 中定义）"
    )
}

/// 解析 api_key：CLI 参数 > 环境变量 > `providers.<名字>.api_key` > 平铺配置文件。
pub fn resolve_api_key(
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
/// 目录不可用时告警并返回 `None`（调用方落到中性兜底）。
pub async fn load_catalog_unless_complete(
    config: Option<&Config>,
    provider_kind: Option<&str>,
    model_id_hint: Option<&str>,
) -> Option<Catalog> {
    let complete = provider_kind.is_some_and(|provider| {
        model_spec_from_config(config, provider, model_id_hint).is_some_and(ModelSpec::is_complete)
    });
    if complete {
        return None;
    }
    let catalog = nomic_ai::models_dev::load().await;
    if catalog.is_none() {
        eprintln!("\x1b[33m⚠ models.dev 目录不可用，模型规格回落到中性兜底值\x1b[0m");
    }
    catalog
}

/// 分层解析的最底层：协议级默认 base URL 与保守的规格兜底值（全零）。
struct Preset {
    /// 默认 base URL（按 API 协议；provider 本身无内置地址）
    default_base_url: &'static str,
    /// 规格兜底值（除 `name` 外全字段有值；`name` 缺省回退为模型 id）
    spec: ModelSpec,
}

/// 中性兜底：规格字段全为保守值，base URL 按 API 协议取官方地址。
const fn neutral_preset(api: ApiKind) -> Preset {
    Preset {
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
    pub fn new(
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
    pub const fn config(&self) -> Option<&Config> {
        self.config.as_ref()
    }

    /// 指定 provider 的配置表定义。
    pub fn provider_config(&self, provider: &str) -> Option<&ProviderConfig> {
        provider_config(self.config(), provider)
    }

    /// provider 的 API 种类：配置显式指定 > 按名推断（anthropic / openai）；
    /// 其余名字必须在配置中定义。
    fn api(&self, provider: &str) -> Result<ApiKind> {
        self.provider_config(provider)
            .and_then(|p| p.api)
            .or_else(|| crate::config::infer_api(provider))
            .with_context(|| {
                format!(
                    "未知 provider {provider:?}：请在 config.toml 的 [providers.{provider}] 中\
                     定义并指定 api（anthropic / openai 可按名自动推断）"
                )
            })
    }

    /// base_url 永远来自用户指定：CLI > 环境变量（仅 openai 系）>
    /// `providers.<名字>.*` > 平铺配置 > 协议默认地址，不经由 models.dev。
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

    /// 规格字段（`name` / `reasoning` / `context_window` / `max_tokens` / `cost_*`）
    /// 逐字段分层：配置 `providers.<名字>.models.<模型id>` > models.dev > 中性兜底。
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
    /// 校验模型存在（禁止配置不存在的模型）：配置覆盖表 / models.dev 目录必须
    /// 命中其一，否则报错——启动路径直接失败，`/models` 运行时切换转为提示。
    /// 目录不可用（离线）时无法校验，保持既有降级行为。
    pub fn resolve(&self, provider: &str, model_id: &str) -> Result<Model> {
        let api = self.api(provider)?;
        let preset = neutral_preset(api);
        self.ensure_known(provider, model_id)?;
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

    /// 模型存在性校验：配置覆盖表（用户显式定义）→ models.dev 目录。
    ///
    /// 目录不可用（离线 / 配置已写全规格跳过加载）时不校验——没有权威数据源
    /// 无法判断「不存在」，维持启动告警 + 中性兜底的降级语义。
    fn ensure_known(&self, provider: &str, model_id: &str) -> Result<()> {
        if model_spec_from_config(self.config(), provider, Some(model_id)).is_some()
            || self
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.lookup(Some(provider), model_id).is_some())
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

    /// 候选 provider 列表：配置表 `[providers]` 定义的名字，按名排序。
    pub fn providers(&self) -> Vec<String> {
        self.config()
            .and_then(|c| c.providers.as_ref())
            .map(|providers| providers.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// `/models` 选择器候选（跨 provider）：每个 provider 的 配置覆盖 ∪
    /// models.dev 目录 ∪ 当前模型；provider 间按名排序（当前模型所在的
    /// provider 未在配置中定义时补入，保证当前模型始终可见）、provider 内
    /// 按模型 id 排序去重。
    ///
    /// 目录不可用（启动时已告警）或 provider 名不命中 models.dev 时，该
    /// provider 只剩配置覆盖与当前模型；`/models:<p>/<id>` 直接切换不受候选
    /// 范围限制。api 解析失败的 provider 整组跳过。
    pub fn candidates(&self, current: &ModelSelection) -> Vec<ModelChoice> {
        let mut choices = Vec::new();
        let mut providers = self.providers();
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
            if let Some(models) = self
                .provider_config(&provider)
                .and_then(|p| p.models.as_ref())
            {
                ids.extend(models.keys().cloned());
            }
            if let Some(catalog) = &self.catalog {
                ids.extend(
                    catalog
                        .models_of(&provider)
                        .into_iter()
                        .map(|(id, _)| id.to_string()),
                );
            }
            choices.extend(ids.into_iter().map(|id| {
                let spec = self.spec_for(&provider, &id, &preset);
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
    let model_id = match &cli.model {
        Some(spec) => ModelSelection::parse(spec, Some(provider_kind))?.model,
        None => bail!("provider {provider_kind:?} 无默认模型，请用 --model 指定模型 id"),
    };
    resolver.resolve(provider_kind, &model_id)
}

#[cfg(test)]
mod tests;
