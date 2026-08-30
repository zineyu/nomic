//! 设置快照（ADR-0039）：sqlite `providers` / `model_specs` / `settings`
//! 三表在启动时读出的内存形态，替代 config.toml。`ModelResolver` 与
//! bootstrap 经它做分层解析；写路径（`nomic config` / TUI `/config` /
//! web WS 事件）落库后经 [`crate::model::ModelResolver::reload`] 刷新快照。
//!
//! 读取宽容、写入严格：快照加载时单键类型不符只告警跳过（启动不因此
//! 失败）；所有写入经 [`validate_scalar`] / [`validate_alias`] /
//! [`validate_provider_name`] 校验后才落库。

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use nomic_ai::{ApiKind, ModelSpec};
use nomic_session::{ProviderRow, SessionStore};

/// 按 provider 名推断 API 种类；anthropic / openai 以外的名字返回 `None`
/// （需在 providers 表中显式设置 `api`）。
pub fn infer_api(provider: &str) -> Option<ApiKind> {
    match provider {
        "anthropic" => Some(ApiKind::AnthropicMessages),
        "openai" => Some(ApiKind::OpenAiCompletions),
        _ => None,
    }
}

/// 标量设置键（`settings` 表）。
pub mod keys {
    /// 全局 base_url 兜底（低于 providers 表）
    pub const BASE_URL: &str = "base_url";
    /// 全局 api_key 兜底（低于 providers 表与环境变量）
    pub const API_KEY: &str = "api_key";
    /// 采样温度
    pub const TEMPERATURE: &str = "temperature";
    /// 单次请求最大输出 token 数
    pub const MAX_TOKENS: &str = "max_tokens";
    /// 追加到系统提示词末尾的文本
    pub const APPEND_SYSTEM: &str = "append_system";
    /// 额外的 prompt template 文件或目录（优先级高于自动发现目录）
    pub const PROMPTS: &str = "prompts";
    /// 自动压缩开关
    pub const COMPACTION_ENABLED: &str = "compaction.enabled";
    /// 为模型响应预留的 token 数
    pub const COMPACTION_RESERVE_TOKENS: &str = "compaction.reserve_tokens";
    /// 保留不压缩的近期 token 数
    pub const COMPACTION_KEEP_RECENT_TOKENS: &str = "compaction.keep_recent_tokens";
    /// 模型别名表（别名 → `<provider>/<模型id>`；子 agent 模型选择用）
    pub const MODEL_ALIASES: &str = "model_aliases";

    /// 全部已知键（`nomic config list` 与写入校验用）
    pub const ALL: &[&str] = &[
        BASE_URL,
        API_KEY,
        TEMPERATURE,
        MAX_TOKENS,
        APPEND_SYSTEM,
        PROMPTS,
        COMPACTION_ENABLED,
        COMPACTION_RESERVE_TOKENS,
        COMPACTION_KEEP_RECENT_TOKENS,
        MODEL_ALIASES,
    ];
}

/// 上下文压缩设置覆盖（`compaction.*` 三个标量键），未设置的字段取内置默认。
#[derive(Debug, Clone, Default)]
pub struct CompactionOverride {
    /// 是否启用自动压缩（手动 `/compact` 不受此开关影响）
    pub enabled: Option<bool>,
    /// 为模型响应预留的 token 数
    pub reserve_tokens: Option<u64>,
    /// 保留不压缩的近期 token 数（估算口径）
    pub keep_recent_tokens: Option<u64>,
}

impl CompactionOverride {
    /// 合并为 core 的压缩配置：未指定字段取内置默认。
    pub fn settings(&self) -> nomic_core::CompactionSettings {
        let defaults = nomic_core::CompactionSettings::default();
        nomic_core::CompactionSettings {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            reserve_tokens: self.reserve_tokens.unwrap_or(defaults.reserve_tokens),
            keep_recent_tokens: self
                .keep_recent_tokens
                .unwrap_or(defaults.keep_recent_tokens),
        }
    }
}

/// 设置快照：三表在某一时刻的完整读出（进程级共享，写后整体刷新）。
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// provider 定义表（`providers` 表，按名索引）
    pub providers: BTreeMap<String, ProviderRow>,
    /// 模型规格覆盖表（`model_specs` 表，(provider, 模型id) 索引）
    pub model_specs: BTreeMap<(String, String), ModelSpec>,
    /// 全局 base_url 兜底
    pub base_url: Option<String>,
    /// 全局 api_key 兜底
    pub api_key: Option<String>,
    /// 采样温度
    pub temperature: Option<f64>,
    /// 单次请求最大输出 token 数
    pub max_tokens: Option<u64>,
    /// 追加到系统提示词末尾的文本
    pub append_system: Option<String>,
    /// 额外的 prompt template 文件或目录
    pub prompts: Vec<PathBuf>,
    /// 上下文压缩覆盖
    pub compaction: CompactionOverride,
    /// 模型别名表（别名 → `<provider>/<模型id>`）
    pub model_aliases: BTreeMap<String, String>,
}

/// 读取单个标量键；库错误或类型不符时告警并返回 `None`（宽容读取：
/// 启动不因一个坏键失败）。
async fn get_scalar<T: serde::de::DeserializeOwned>(store: &SessionStore, key: &str) -> Option<T> {
    match store.get_setting::<T>(key).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(error = ?error, key = %key, "读取设置失败，按未设置处理");
            None
        }
    }
}

impl Settings {
    /// 从 store 加载快照；store 不可用返回空快照（与既有 store 不可用的
    /// 降级语义一致：不持久化、无设置层）。
    pub async fn load(store: Option<&SessionStore>) -> Self {
        let Some(store) = store else {
            return Self::default();
        };
        let providers = match store.list_providers().await {
            Ok(rows) => rows
                .into_iter()
                .map(|row| (row.name.clone(), row))
                .collect(),
            Err(error) => {
                tracing::warn!(error = ?error, "读取 providers 表失败，按空表处理");
                BTreeMap::new()
            }
        };
        let model_specs = match store.list_model_specs().await {
            Ok(rows) => rows
                .into_iter()
                .map(|row| ((row.provider.clone(), row.model_id.clone()), row.spec))
                .collect(),
            Err(error) => {
                tracing::warn!(error = ?error, "读取 model_specs 表失败，按空表处理");
                BTreeMap::new()
            }
        };
        Self {
            providers,
            model_specs,
            base_url: get_scalar(store, keys::BASE_URL).await,
            api_key: get_scalar(store, keys::API_KEY).await,
            temperature: get_scalar(store, keys::TEMPERATURE).await,
            max_tokens: get_scalar(store, keys::MAX_TOKENS).await,
            append_system: get_scalar(store, keys::APPEND_SYSTEM).await,
            prompts: get_scalar(store, keys::PROMPTS).await.unwrap_or_default(),
            compaction: CompactionOverride {
                enabled: get_scalar(store, keys::COMPACTION_ENABLED).await,
                reserve_tokens: get_scalar(store, keys::COMPACTION_RESERVE_TOKENS).await,
                keep_recent_tokens: get_scalar(store, keys::COMPACTION_KEEP_RECENT_TOKENS).await,
            },
            model_aliases: get_scalar(store, keys::MODEL_ALIASES)
                .await
                .unwrap_or_default(),
        }
    }
}

/// 校验 provider 名：非空、不含 `/` 与空白（作为 `<provider>/<模型id>`
/// 选择项的前段与 WS/CLI 参数传输）。
pub fn validate_provider_name(name: &str) -> Result<()> {
    if name.is_empty() || name.chars().any(|c| c == '/' || c.is_whitespace()) {
        bail!("provider 名 {name:?} 非法：非空且不能含 / 或空白字符");
    }
    Ok(())
}

/// 写入 provider 前的校验（CLI / TUI / web 共用）：名字合法；新建时 api
/// 必须可解析（补丁显式给出或按名推断），避免存下永远解析失败的定义。
pub async fn validate_provider_patch(
    store: &SessionStore,
    name: &str,
    patch: &nomic_session::ProviderPatch,
) -> Result<()> {
    validate_provider_name(name)?;
    let creating = store.get_provider(name).await?.is_none();
    let effective_api = patch.api.as_ref().copied().flatten();
    if creating && effective_api.is_none() && infer_api(name).is_none() {
        bail!(
            "自定义 provider {name:?} 必须指定 api（--api anthropic_messages / open_ai_completions）"
        );
    }
    Ok(())
}

/// 校验模型别名：名字为 URL/参数友好的短标识（字母数字、`-`、`_`），
/// 目标为 `<provider>/<模型id>` 全形式（别名解析不经默认 provider 上下文）。
pub fn validate_alias(alias: &str, spec: &str) -> Result<()> {
    if alias.is_empty()
        || !alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "模型别名 {alias:?} 非法：只允许字母、数字、-、_\
             （按模型能力命名，如 smart / fast / vision）"
        );
    }
    match spec.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {}
        _ => bail!("模型别名 {alias:?} 的目标 {spec:?} 非法：应为 <provider>/<模型id> 格式"),
    }
    Ok(())
}

/// 校验标量设置的键与取值类型（写入路径共用：CLI / TUI `/config` /
/// web WS）。未知键硬报错（防拼写错误，与旧 config.toml
/// `deny_unknown_fields` 同一口径）。
pub fn validate_scalar(key: &str, value: &serde_json::Value) -> Result<()> {
    let ok = match key {
        keys::BASE_URL | keys::API_KEY | keys::APPEND_SYSTEM => value.is_string(),
        keys::TEMPERATURE => value.as_f64().is_some(),
        keys::MAX_TOKENS
        | keys::COMPACTION_RESERVE_TOKENS
        | keys::COMPACTION_KEEP_RECENT_TOKENS => value.as_u64().is_some(),
        keys::COMPACTION_ENABLED => value.is_boolean(),
        keys::PROMPTS => value
            .as_array()
            .is_some_and(|items| items.iter().all(serde_json::Value::is_string)),
        keys::MODEL_ALIASES => {
            let Some(map) = value.as_object() else {
                bail!("设置 {key} 应为对象（别名 → <provider>/<模型id>）");
            };
            for (alias, target) in map {
                let target = target
                    .as_str()
                    .with_context(|| format!("模型别名 {alias:?} 的目标应为字符串"))?;
                validate_alias(alias, target)?;
            }
            return Ok(());
        }
        _ => bail!("未知设置键 {key:?}（可选：{}）", keys::ALL.join(" / ")),
    };
    if !ok {
        bail!("设置 {key} 的取值类型非法：{value}");
    }
    Ok(())
}
