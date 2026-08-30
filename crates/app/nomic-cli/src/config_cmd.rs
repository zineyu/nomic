//! `nomic config` 子命令族：设置的读写入口（ADR-0039，落库到 sqlite
//! `providers` / `model_specs` / `settings` 三表）。TUI `/config` 复用
//! 同一套 clap 解析与执行逻辑（[`parse_args`] / [`execute`]）。

use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use nomic_ai::ApiKind;
use nomic_session::{ModelSpecPatch, ProviderPatch, SessionStore};

use crate::model::ModelSelection;
use crate::settings::{infer_api, keys, validate_provider_name, validate_scalar};

/// `nomic config` 子命令。
#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// 列出全部设置（标量 + provider 定义 + 模型规格覆盖）
    List,
    /// 读取标量设置
    Get {
        /// 设置键（如 temperature；可用键见 `nomic config list` 输出）
        key: String,
    },
    /// 写入标量设置（值先按 JSON 解析，非 JSON 时按字符串处理）
    Set {
        /// 设置键
        key: String,
        /// 取值（JSON 或裸字符串）
        value: String,
    },
    /// 删除标量设置（恢复下层默认）
    Unset {
        /// 设置键
        key: String,
    },
    /// 管理 provider 定义
    Providers {
        #[command(subcommand)]
        command: ProvidersCommand,
    },
    /// 管理模型规格覆盖（`<provider>/<模型id>`，逐字段）
    Models {
        #[command(subcommand)]
        command: ModelsCommand,
    },
}

/// `nomic config providers` 子命令。
#[derive(Debug, Clone, Subcommand)]
pub enum ProvidersCommand {
    /// 列出全部 provider 定义
    List,
    /// 新建或更新 provider（只更新显式传入的字段）
    Set {
        /// provider 名
        name: String,
        /// API 种类：anthropic_messages / open_ai_completions
        /// （anthropic、openai 可按名推断，可省略）
        #[arg(long, value_parser = ["anthropic_messages", "open_ai_completions"])]
        api: Option<String>,
        /// API base URL
        #[arg(long)]
        base_url: Option<String>,
        /// API key（建议优先用环境变量，避免明文落库）
        #[arg(long)]
        api_key: Option<String>,
        /// 清除字段（可重复）：api / base-url / api-key
        #[arg(long, value_parser = ["api", "base-url", "api-key"])]
        clear: Vec<String>,
    },
    /// 删除 provider（其模型规格覆盖级联清除）
    Unset {
        /// provider 名
        name: String,
    },
}

/// `nomic config models` 子命令。
#[derive(Debug, Clone, Subcommand)]
pub enum ModelsCommand {
    /// 列出全部模型规格覆盖
    List,
    /// 新建或更新模型规格覆盖（只更新显式传入的字段）
    Set {
        /// `<provider>/<模型id>`
        spec: String,
        /// 展示名
        #[arg(long)]
        name: Option<String>,
        /// 是否支持推理/思考
        #[arg(long)]
        reasoning: Option<bool>,
        /// 是否支持图像输入（多模态）
        #[arg(long)]
        vision: Option<bool>,
        /// 上下文窗口 token 数
        #[arg(long)]
        context_window: Option<u64>,
        /// 最大输出 token 数
        #[arg(long)]
        max_tokens: Option<u64>,
        /// 每百万 token 费率：输入
        #[arg(long)]
        cost_input: Option<f64>,
        /// 每百万 token 费率：输出
        #[arg(long)]
        cost_output: Option<f64>,
        /// 每百万 token 费率：缓存读取
        #[arg(long)]
        cost_cache_read: Option<f64>,
        /// 每百万 token 费率：缓存写入
        #[arg(long)]
        cost_cache_write: Option<f64>,
        /// 清除字段（可重复）：name / reasoning / vision / context-window /
        /// max-tokens / cost-input / cost-output / cost-cache-read / cost-cache-write
        #[arg(
            long,
            value_parser = ["name", "reasoning", "vision", "context-window", "max-tokens", "cost-input", "cost-output", "cost-cache-read", "cost-cache-write"]
        )]
        clear: Vec<String>,
    },
    /// 删除模型规格覆盖（恢复 models.dev / 中性兜底解析）
    Unset {
        /// `<provider>/<模型id>`
        spec: String,
    },
}

/// 供 TUI `/config` 复用的解析入口：把命令参数序列解析为 [`ConfigCommand`]。
// 消费者随 TUI `/config` 任务落地
#[expect(dead_code)]
pub fn parse_args(args: &[&str]) -> Result<ConfigCommand> {
    use clap::Parser as _;
    #[derive(clap::Parser)]
    struct ConfigArgs {
        #[command(subcommand)]
        command: ConfigCommand,
    }
    Ok(ConfigArgs::try_parse_from(std::iter::once("config").chain(args.iter().copied()))?.command)
}

/// CLI 入口：打开默认库执行并打印结果。
pub async fn run(command: &ConfigCommand) -> Result<()> {
    let store = SessionStore::open_default()
        .await
        .context("打开 session 库失败")?;
    let output = execute(command, &store).await?;
    println!("{output}");
    Ok(())
}

/// 执行子命令（在指定 store 上），返回给用户展示的文本。
pub async fn execute(command: &ConfigCommand, store: &SessionStore) -> Result<String> {
    match command {
        ConfigCommand::List => list_all(store).await,
        ConfigCommand::Get { key } => get_scalar(store, key).await,
        ConfigCommand::Set { key, value } => set_scalar(store, key, value).await,
        ConfigCommand::Unset { key } => unset_scalar(store, key).await,
        ConfigCommand::Providers { command } => providers(command, store).await,
        ConfigCommand::Models { command } => models(command, store).await,
    }
}

// ── 标量设置 ──────────────────────────────────────────────────────────

/// 校验键已知（get/unset 也校验：拼写错误应硬报错而非读到「未设置」）。
fn validate_known_key(key: &str) -> Result<()> {
    if !keys::ALL.contains(&key) {
        bail!("未知设置键 {key:?}（可选：{}）", keys::ALL.join(" / "));
    }
    Ok(())
}

/// 值解析：先按 JSON，失败按裸字符串（`set append_system 总是用中文回复`
/// 这类自然输入仍是字符串）。
fn parse_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

async fn get_scalar(store: &SessionStore, key: &str) -> Result<String> {
    validate_known_key(key)?;
    match store.get_setting::<serde_json::Value>(key).await? {
        Some(value) => Ok(serde_json::to_string_pretty(&value)?),
        None => Ok(format!("{key} 未设置")),
    }
}

async fn set_scalar(store: &SessionStore, key: &str, raw: &str) -> Result<String> {
    let value = parse_value(raw);
    validate_scalar(key, &value)?;
    store.set_setting(key, &value).await?;
    Ok(format!("已设置 {key} = {value}"))
}

async fn unset_scalar(store: &SessionStore, key: &str) -> Result<String> {
    validate_known_key(key)?;
    if store.unset_setting(key).await? {
        Ok(format!("已删除 {key}"))
    } else {
        Ok(format!("{key} 本未设置"))
    }
}

// ── providers ─────────────────────────────────────────────────────────

fn parse_api(value: &str) -> Result<ApiKind> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .with_context(|| format!("api 取值非法：{value:?}"))
}

async fn providers(command: &ProvidersCommand, store: &SessionStore) -> Result<String> {
    match command {
        ProvidersCommand::List => {
            let rows = store.list_providers().await?;
            if rows.is_empty() {
                return Ok("没有 provider 定义（用 nomic config providers set 新建）。".to_string());
            }
            let mut out = String::new();
            for row in rows {
                let api = row.api.map_or_else(
                    || "按名推断".to_string(),
                    |api| {
                        serde_json::to_value(api)
                            .expect("ApiKind 序列化")
                            .to_string()
                    },
                );
                let _ = writeln!(out, "{}", row.name);
                let _ = writeln!(out, "  api: {}", api.trim_matches('"'));
                if let Some(base_url) = &row.base_url {
                    let _ = writeln!(out, "  base_url: {base_url}");
                }
                let _ = writeln!(
                    out,
                    "  api_key: {}",
                    if row.api_key.is_some() {
                        "已设置"
                    } else {
                        "未设置"
                    }
                );
            }
            Ok(out.trim_end().to_string())
        }
        ProvidersCommand::Set {
            name,
            api,
            base_url,
            api_key,
            clear,
        } => {
            validate_provider_name(name)?;
            let mut patch = ProviderPatch::default();
            if let Some(api) = api {
                if clear.contains(&"api".to_string()) {
                    bail!("--api 与 --clear api 不能同时指定");
                }
                patch.api = Some(Some(parse_api(api)?));
            }
            if let Some(base_url) = base_url {
                if clear.contains(&"base-url".to_string()) {
                    bail!("--base-url 与 --clear base-url 不能同时指定");
                }
                patch.base_url = Some(Some(base_url.clone()));
            }
            if let Some(api_key) = api_key {
                if clear.contains(&"api-key".to_string()) {
                    bail!("--api-key 与 --clear api-key 不能同时指定");
                }
                patch.api_key = Some(Some(api_key.clone()));
            }
            for field in clear {
                match field.as_str() {
                    "api" => patch.api = Some(None),
                    "base-url" => patch.base_url = Some(None),
                    "api-key" => patch.api_key = Some(None),
                    other => bail!("未知清除字段 {other:?}"),
                }
            }
            // 新建 provider 时校验 api 可解析（按名推断或显式给出），
            // 避免存下一条永远解析失败的定义
            let creating = store.get_provider(name).await?.is_none();
            let effective_api = patch.api.as_ref().copied().flatten();
            if creating && effective_api.is_none() && infer_api(name).is_none() {
                bail!("自定义 provider {name:?} 必须用 --api 指定 API 种类");
            }
            store.upsert_provider(name, patch).await?;
            Ok(format!("已保存 provider {name}"))
        }
        ProvidersCommand::Unset { name } => {
            if store.delete_provider(name).await? {
                Ok(format!("已删除 provider {name}（其模型规格覆盖一并清除）"))
            } else {
                Ok(format!("provider {name} 不存在"))
            }
        }
    }
}

// ── models ────────────────────────────────────────────────────────────

async fn models(command: &ModelsCommand, store: &SessionStore) -> Result<String> {
    match command {
        ModelsCommand::List => {
            let rows = store.list_model_specs().await?;
            if rows.is_empty() {
                return Ok(
                    "没有模型规格覆盖（用 nomic config models set <provider>/<模型id> 新建）。"
                        .to_string(),
                );
            }
            let mut out = String::new();
            for row in rows {
                let _ = writeln!(out, "{}/{}", row.provider, row.model_id);
                let spec = serde_json::to_value(&row.spec).expect("ModelSpec 序列化");
                for (field, value) in spec.as_object().expect("ModelSpec 为对象") {
                    if !value.is_null() {
                        let _ = writeln!(out, "  {field}: {value}");
                    }
                }
            }
            Ok(out.trim_end().to_string())
        }
        ModelsCommand::Set {
            spec,
            name,
            reasoning,
            vision,
            context_window,
            max_tokens,
            cost_input,
            cost_output,
            cost_cache_read,
            cost_cache_write,
            clear,
        } => {
            let selection = ModelSelection::parse(spec, None)
                .with_context(|| format!("模型选择项 {spec:?} 非法"))?;
            if store.get_provider(&selection.provider).await?.is_none() {
                bail!(
                    "provider {:?} 未定义：先用 nomic config providers set {} 创建",
                    selection.provider,
                    selection.provider
                );
            }
            let mut patch = ModelSpecPatch {
                name: name.clone().map(Some),
                reasoning: reasoning.map(Some),
                vision: vision.map(Some),
                context_window: context_window.map(Some),
                max_tokens: max_tokens.map(Some),
                cost_input: cost_input.map(Some),
                cost_output: cost_output.map(Some),
                cost_cache_read: cost_cache_read.map(Some),
                cost_cache_write: cost_cache_write.map(Some),
            };
            for field in clear {
                match field.as_str() {
                    "name" => patch.name = Some(None),
                    "reasoning" => patch.reasoning = Some(None),
                    "vision" => patch.vision = Some(None),
                    "context-window" => patch.context_window = Some(None),
                    "max-tokens" => patch.max_tokens = Some(None),
                    "cost-input" => patch.cost_input = Some(None),
                    "cost-output" => patch.cost_output = Some(None),
                    "cost-cache-read" => patch.cost_cache_read = Some(None),
                    "cost-cache-write" => patch.cost_cache_write = Some(None),
                    other => bail!("未知清除字段 {other:?}"),
                }
            }
            store
                .upsert_model_spec(&selection.provider, &selection.model, patch)
                .await?;
            Ok(format!("已保存模型规格覆盖 {}", selection.spec()))
        }
        ModelsCommand::Unset { spec } => {
            let selection = ModelSelection::parse(spec, None)
                .with_context(|| format!("模型选择项 {spec:?} 非法"))?;
            if store
                .delete_model_spec(&selection.provider, &selection.model)
                .await?
            {
                Ok(format!("已删除模型规格覆盖 {}", selection.spec()))
            } else {
                Ok(format!("模型规格覆盖 {} 不存在", selection.spec()))
            }
        }
    }
}

// ── 全量列表 ──────────────────────────────────────────────────────────

async fn list_all(store: &SessionStore) -> Result<String> {
    let mut out = String::new();
    let _ = writeln!(out, "标量设置（可用键：{}）", keys::ALL.join(" / "));
    let settings = store.list_settings().await?;
    if settings.is_empty() {
        let _ = writeln!(out, "  （空）");
    }
    for (key, value) in settings {
        let _ = writeln!(out, "  {key} = {value}");
    }
    out.push('\n');
    let _ = writeln!(out, "providers：");
    out.push_str(&providers(&ProvidersCommand::List, store).await?);
    out.push_str("\n\n模型规格覆盖：\n");
    out.push_str(&models(&ModelsCommand::List, store).await?);
    Ok(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_value_json_then_string_fallback() {
        assert_eq!(parse_value("0.7"), serde_json::json!(0.7));
        assert_eq!(parse_value("true"), serde_json::json!(true));
        assert_eq!(parse_value("[\"a\"]"), serde_json::json!(["a"]));
        assert_eq!(
            parse_value("总是用中文回复"),
            serde_json::json!("总是用中文回复")
        );
        assert_eq!(
            parse_value("https://gw/v1"),
            serde_json::json!("https://gw/v1")
        );
    }

    #[tokio::test]
    async fn scalar_set_get_unset_via_execute() {
        let store = SessionStore::in_memory().await.expect("store");
        let out = execute(
            &ConfigCommand::Set {
                key: "temperature".to_string(),
                value: "0.7".to_string(),
            },
            &store,
        )
        .await
        .expect("set");
        assert!(out.contains("0.7"));
        let out = execute(
            &ConfigCommand::Get {
                key: "temperature".to_string(),
            },
            &store,
        )
        .await
        .expect("get");
        assert!(out.contains("0.7"));
        let out = execute(
            &ConfigCommand::Unset {
                key: "temperature".to_string(),
            },
            &store,
        )
        .await
        .expect("unset");
        assert!(out.contains("已删除"));
        let out = execute(
            &ConfigCommand::Get {
                key: "temperature".to_string(),
            },
            &store,
        )
        .await
        .expect("get");
        assert!(out.contains("未设置"));
    }

    #[tokio::test]
    async fn unknown_key_rejected_everywhere() {
        let store = SessionStore::in_memory().await.expect("store");
        for command in [
            ConfigCommand::Get {
                key: "no_such_key".to_string(),
            },
            ConfigCommand::Set {
                key: "no_such_key".to_string(),
                value: "1".to_string(),
            },
            ConfigCommand::Unset {
                key: "no_such_key".to_string(),
            },
        ] {
            let error = execute(&command, &store).await.expect_err("未知键必须报错");
            assert!(format!("{error:#}").contains("未知设置键"));
        }
    }

    #[tokio::test]
    async fn alias_value_validated_on_write() {
        let store = SessionStore::in_memory().await.expect("store");
        let error = execute(
            &ConfigCommand::Set {
                key: "model_aliases".to_string(),
                value: r#"{"my alias!": "openai/gpt-4o"}"#.to_string(),
            },
            &store,
        )
        .await
        .expect_err("非法别名必须报错");
        assert!(format!("{error:#}").contains("my alias!"));
        execute(
            &ConfigCommand::Set {
                key: "model_aliases".to_string(),
                value: r#"{"smart": "openai/gpt-4o"}"#.to_string(),
            },
            &store,
        )
        .await
        .expect("合法别名可写入");
    }

    #[tokio::test]
    async fn provider_set_validates_api_for_custom_names() {
        let store = SessionStore::in_memory().await.expect("store");
        // 自定义 provider 不给 api：拒绝
        let error = execute(
            &ConfigCommand::Providers {
                command: ProvidersCommand::Set {
                    name: "deepseek".to_string(),
                    api: None,
                    base_url: Some("https://api.deepseek.com/v1".to_string()),
                    api_key: None,
                    clear: Vec::new(),
                },
            },
            &store,
        )
        .await
        .expect_err("自定义 provider 必须给 api");
        assert!(format!("{error:#}").contains("--api"));
        // 给出 api 后可建；models set 依赖 provider 存在
        execute(
            &ConfigCommand::Providers {
                command: ProvidersCommand::Set {
                    name: "deepseek".to_string(),
                    api: Some("open_ai_completions".to_string()),
                    base_url: Some("https://api.deepseek.com/v1".to_string()),
                    api_key: None,
                    clear: Vec::new(),
                },
            },
            &store,
        )
        .await
        .expect("provider 可建");
        execute(
            &ConfigCommand::Models {
                command: ModelsCommand::Set {
                    spec: "deepseek/deepseek-chat".to_string(),
                    name: None,
                    reasoning: Some(false),
                    vision: None,
                    context_window: Some(128_000),
                    max_tokens: None,
                    cost_input: None,
                    cost_output: None,
                    cost_cache_read: None,
                    cost_cache_write: None,
                    clear: Vec::new(),
                },
            },
            &store,
        )
        .await
        .expect("模型规格可建");
        let out = execute(&ConfigCommand::List, &store).await.expect("list");
        assert!(out.contains("deepseek"), "{out}");
        assert!(out.contains("deepseek-chat"), "{out}");
        assert!(out.contains("128000"), "{out}");
    }

    #[tokio::test]
    async fn models_set_requires_existing_provider() {
        let store = SessionStore::in_memory().await.expect("store");
        let error = execute(
            &ConfigCommand::Models {
                command: ModelsCommand::Set {
                    spec: "ghost/m".to_string(),
                    name: None,
                    reasoning: None,
                    vision: None,
                    context_window: None,
                    max_tokens: None,
                    cost_input: None,
                    cost_output: None,
                    cost_cache_read: None,
                    cost_cache_write: None,
                    clear: Vec::new(),
                },
            },
            &store,
        )
        .await
        .expect_err("provider 不存在必须报错");
        assert!(format!("{error:#}").contains("未定义"));
    }
}
