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
    let error =
        resolve_model("gemini", &cli(&[]), None, None, None).expect_err("未知 provider 必须报错");
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
