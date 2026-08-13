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
    ApiKind, Catalog, Model, ModelSpec, Provider,
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

/// sqlite 配置表中模型选择的配置键。
pub const CONFIG_KEY_MODEL: &str = "model";

/// 模型选择项：`<provider>/<模型id>` 格式（sqlite 配置与 `/models` 命令共用）。
#[derive(Debug, Clone, PartialEq, Eq)]
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
        let provider = cli
            .model
            .as_deref()
            .and_then(|spec| {
                spec.split_once('/')
                    .map(|(provider, _)| provider.to_string())
            })
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
mod tests {
    use clap::Parser as _;

    use super::*;

    /// 从 argv 构造 Cli（与真实命令行解析路径一致）。
    fn cli(args: &[&str]) -> Cli {
        Cli::parse_from(std::iter::once("nomic").chain(args.iter().copied()))
    }

    // ── 配置分层：CLI > 环境变量 > 配置文件 > 协议默认 ─────────────────────

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
    fn cli_model_beats_db_history() {
        // CLI 选择器生效时数据库选择整层不生效
        let cli = cli(&["--model", "openai/gpt-5.2"]);
        let resolver = resolver(&Cli::parse_from(["nomic"]), None, Some(catalog()));
        let db_history = [ModelSelection {
            provider: "openai".to_string(),
            model: "other-model".to_string(),
        }];
        let model = select_startup_model(&cli, &db_history, &resolver).expect("select");
        assert_eq!(model.id, "gpt-5.2");
    }

    #[test]
    fn db_history_feedback_falls_back_to_older_selection() {
        // 最新选择已失效（模型不在目录/配置中）：告警后回退到更早的可解析选择
        let resolver = resolver(&cli(&[]), None, Some(catalog()));
        let db_history = [
            ModelSelection {
                provider: "openai".to_string(),
                model: "gpt-retired".to_string(),
            },
            ModelSelection {
                provider: "openai".to_string(),
                model: "gpt-5.2".to_string(),
            },
        ];
        let model = select_startup_model(&cli(&[]), &db_history, &resolver).expect("select");
        assert_eq!(model.id, "gpt-5.2", "第一条可解析的选择生效");
    }

    #[test]
    fn cli_model_without_provider_is_rejected() {
        // 裸模型 id 且无 --provider：无内置默认 provider，必须报错
        let cli = cli(&["--model", "gpt-5.2"]);
        let resolver = resolver(&Cli::parse_from(["nomic"]), None, Some(catalog()));
        let error = select_startup_model(&cli, &[], &resolver).expect_err("缺 provider 必须报错");
        assert!(format!("{error:#}").contains("缺 provider"));
    }

    #[test]
    fn cli_provider_without_model_is_rejected() {
        // 只给 --provider：无内置默认模型，必须显式指定模型
        let cli = cli(&["--provider", "openai"]);
        let resolver = resolver(&Cli::parse_from(["nomic"]), None, Some(catalog()));
        let error = select_startup_model(&cli, &[], &resolver).expect_err("缺模型必须报错");
        assert!(format!("{error:#}").contains("无默认模型"));
    }

    #[test]
    fn db_history_exhausted_requires_explicit_model() {
        // 回退链全部失效：无内置默认模型可落，报错要求显式指定
        let resolver = resolver(&cli(&[]), None, Some(catalog()));
        let db_history = [ModelSelection {
            provider: "openai".to_string(),
            model: "gpt-retired".to_string(),
        }];
        let error =
            select_startup_model(&cli(&[]), &db_history, &resolver).expect_err("链尽时必须报错");
        assert!(format!("{error:#}").contains("未指定模型"));
    }

    #[test]
    fn model_selection_parse() {
        let full = ModelSelection::parse("openai/gpt-5.2", None).expect("full form");
        assert_eq!(full.provider, "openai");
        assert_eq!(full.model, "gpt-5.2");
        assert_eq!(full.spec(), "openai/gpt-5.2");
        // 模型 id 自身含 /（openrouter 风格）：按第一个 / 切分
        let nested = ModelSelection::parse("openrouter/openai/gpt-4o", None).expect("nested");
        assert_eq!(nested.provider, "openrouter");
        assert_eq!(nested.model, "openai/gpt-4o");
        // 裸模型 id 用默认 provider
        let bare = ModelSelection::parse("gpt-5.2", Some("openai")).expect("bare");
        assert_eq!(bare.provider, "openai");
        // 裸模型 id 无默认 provider、空 provider、空模型 id：均报错
        assert!(ModelSelection::parse("gpt-5.2", None).is_err());
        assert!(ModelSelection::parse("/gpt-5.2", None).is_err());
        assert!(ModelSelection::parse("openai/", None).is_err());
        assert!(ModelSelection::parse("", Some("openai")).is_err());
    }

    #[test]
    fn cli_model_accepts_provider_qualified_form() {
        // --model 的 <provider>/<模型id> 全形式跨 provider 指定
        let cli = cli(&["--model", "deepseek/deepseek-chat"]);
        let config = deepseek_config();
        let resolver = resolver(&Cli::parse_from(["nomic"]), Some(config), Some(catalog()));
        let model = select_startup_model(&cli, &[], &resolver).expect("select");
        assert_eq!(model.provider, "deepseek");
        assert_eq!(model.id, "deepseek-chat");
    }

    #[test]
    fn base_url_precedence_cli_env_config_default() {
        let config = Config {
            base_url: Some("https://config".to_string()),
            ..Config::default()
        };
        let with_flag = cli(&["--base-url", "https://cli", "--model", "gpt-5.2"]);
        let plain = cli(&["--model", "gpt-5.2"]);
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
        // 协议默认地址兜底
        let model = resolve("openai", &plain, None, None, None);
        assert_eq!(model.base_url, "https://api.openai.com/v1");
        // OPENAI_BASE_URL 对 anthropic 不生效
        let anthropic = cli(&["--model", "claude-sonnet-4-5"]);
        let model = resolve("anthropic", &anthropic, None, Some("https://env"), None);
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
        let model = resolve(
            "openai",
            &cli(&["--model", "gpt-5.2"]),
            Some(&config),
            None,
            None,
        );
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

    // ── 规格字段分层：配置 > models.dev > 中性兜底 ─────────────────────────

    #[test]
    fn spec_from_catalog_fills_fields_neutral_preset_is_last_resort() {
        let with_model = cli(&["--model", "gpt-5.2"]);
        // models.dev 命中：gpt-5.2 有 limit 但无 cost → cost 落中性兜底（全零）
        let model = resolve("openai", &with_model, None, None, Some(&catalog()));
        assert_eq!(model.id, "gpt-5.2");
        assert_eq!(model.name, "GPT-5.2", "展示名来自 models.dev");
        assert_eq!(model.context_window, 400_000);
        assert_eq!(model.max_tokens, 128_000);
        assert!(model.reasoning);
        assert_eq!(
            Some(model.cost_input),
            Some(0.0),
            "models.dev 缺 cost 时落中性兜底"
        );
        // 无 models.dev：规格字段全部落中性兜底（目录不可用时不校验存在性）
        let model = resolve("openai", &with_model, None, None, None);
        assert_eq!(model.name, "gpt-5.2", "name 兜底为模型 id");
        assert_eq!(model.context_window, 0);
        let claude = cli(&["--model", "claude-sonnet-4-5"]);
        let model = resolve("anthropic", &claude, None, None, None);
        assert_eq!(model.context_window, 0);
        assert_eq!(Some(model.cost_input), Some(0.0));
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
        let model = resolve(
            "openai",
            &cli(&["--model", "gpt-5.2"]),
            Some(&config),
            None,
            Some(&catalog()),
        );
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
            providers: Some(providers),
            ..Config::default()
        }
    }

    #[test]
    fn custom_provider_resolves_via_config_and_global_catalog_scan() {
        let config = deepseek_config();
        // 即便 provider 名不是 models.dev 的一级键，也按模型 id 全局扫描命中
        let cli = cli(&["--model", "deepseek-chat"]);
        let model = resolve("deepseek", &cli, Some(&config), None, Some(&catalog()));
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
        let cli = cli(&["--model", "deepseek-chat"]);
        let model = resolve("deepseek", &cli, Some(&config), None, None);
        assert_eq!(model.name, "deepseek-chat");
        assert_eq!(model.context_window, 0);
        assert!(!model.reasoning);
        assert_eq!(Some(model.cost_input), Some(0.0));
    }

    #[test]
    fn custom_provider_requires_explicit_model() {
        let config = deepseek_config();
        let error = resolve_model("deepseek", &cli(&[]), Some(&config), None, None)
            .expect_err("无默认模型，必须显式指定");
        assert!(format!("{error:#}").contains("无默认模型"));
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
        // 与启动解析同一口径：配置覆盖 > models.dev > 中性兜底
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
        assert_eq!(model.context_window, 0, "规格字段仍按分层解析");
    }

    #[test]
    fn unknown_model_degraded_when_catalog_unavailable() {
        // 离线降级：目录不可用无法校验存在性，保持既有回落行为（启动告警已发）
        let model = resolve_model("openai", &cli(&["--model", "gpt-future"]), None, None, None)
            .expect("目录不可用时未知模型回落中性兜底");
        assert_eq!(model.id, "gpt-future");
        assert_eq!(model.context_window, 0);
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
        let current = ModelSelection::parse("openai/gpt-future", None).expect("current");
        let choices = resolver.candidates(&current);
        let specs: Vec<String> = choices.iter().map(ModelChoice::spec).collect();
        assert_eq!(
            specs,
            ["openai/gpt-5.2", "openai/gpt-future", "openai/my-fine-tune",],
            "每个 provider 的 目录 ∪ 当前模型 ∪ 配置覆盖"
        );
        let gpt = &choices[0];
        assert_eq!(gpt.name, "GPT-5.2");
        assert_eq!(gpt.context_window, 400_000);
        assert!(gpt.reasoning, "gpt-5.2 的推理能力来自 models.dev");
        assert_eq!(choices[2].name, "My Fine Tune", "展示名来自配置覆盖");
        // 重复解析当前模型：候选中再选一次幂等
        let again = resolver.candidates(&ModelSelection::parse("openai/gpt-5.2", None).unwrap());
        assert!(
            !again.iter().any(|choice| choice.id == "gpt-future"),
            "gpt-future 不再是当前模型后不出现"
        );
        assert_eq!(again.len(), 2);
    }

    #[test]
    fn candidates_include_current_provider_not_in_config() {
        // 当前模型的 provider 未在配置中定义（按名推断 api）：补入候选列表，
        // 保证当前模型始终可见
        let resolver = resolver(&cli(&[]), None, Some(catalog()));
        let current = ModelSelection::parse("openai/gpt-5.2", None).expect("current");
        let specs: Vec<String> = resolver
            .candidates(&current)
            .iter()
            .map(ModelChoice::spec)
            .collect();
        assert_eq!(specs, ["openai/gpt-5.2"]);
    }

    #[test]
    fn candidates_without_catalog_keep_config_and_current() {
        // 目录不可用（如配置已写全规格字段跳过加载）：仍列出配置覆盖与当前模型
        let config = deepseek_config();
        let resolver = resolver(&cli(&[]), Some(config), None);
        let current = ModelSelection::parse("deepseek/deepseek-chat", None).expect("current");
        let choices = resolver.candidates(&current);
        let specs: Vec<String> = choices.iter().map(ModelChoice::spec).collect();
        assert_eq!(specs, ["deepseek/deepseek-chat"]);
        let deepseek = &choices[0];
        assert_eq!(deepseek.context_window, 0, "无目录时规格落中性兜底");
        assert!(!deepseek.reasoning, "中性兜底不支持推理");
    }
}
