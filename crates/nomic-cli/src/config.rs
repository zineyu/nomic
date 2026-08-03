//! 用户级配置文件加载：路径解析、TOML 反序列化与字段校验。
//!
//! 配置文件为可选：文件不存在时视为无配置；存在但读取/解析/校验失败时硬报错
//! （用户显式写了配置，静默降级会掩盖拼写错误等问题）。整体优先级为
//! CLI 参数 > 环境变量 > 配置文件 > 内置默认，本模块只负责「配置文件」这一层，
//! 分层合并在 `bootstrap` 完成。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use nomic_ai::{ApiKind, ModelSpec};
use serde::Deserialize;

/// 内置 provider 名（`api` 可省略自动推断）。
const BUILTIN_PROVIDERS: [&str; 2] = ["anthropic", "openai"];

/// 按 provider 名推断 API 种类；内置以外的名字返回 `None`（需在配置中显式指定 `api`）。
pub fn infer_api(provider: &str) -> Option<ApiKind> {
    match provider {
        "anthropic" => Some(ApiKind::AnthropicMessages),
        "openai" => Some(ApiKind::OpenAiCompletions),
        _ => None,
    }
}

/// 用户级配置（`config.toml` 的反序列化目标）。
///
/// 全部字段可选：缺省的字段继续向上取环境变量与内置默认；
/// `deny_unknown_fields` 让未知键硬报错，避免拼写错误被静默忽略。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// provider：anthropic、openai，或下方 `[providers]` 表中定义的自定义名字
    pub provider: Option<String>,
    /// 模型 id
    pub model: Option<String>,
    /// API base URL
    pub base_url: Option<String>,
    /// API key（最低优先级兜底，低于环境变量）
    pub api_key: Option<String>,
    /// 推理级别：minimal/low/medium/high
    pub reasoning: Option<String>,
    /// 采样温度
    pub temperature: Option<f64>,
    /// 最大输出 token 数
    pub max_tokens: Option<u64>,
    /// 追加到系统提示词末尾的文本
    pub append_system: Option<String>,
    /// 额外的 prompt template 文件或目录（优先级高于自动发现的项目/用户目录）
    pub prompts: Option<Vec<PathBuf>>,
    /// provider 定义表（`[providers.<名字>]`，含嵌套的模型规格覆盖）
    pub providers: Option<BTreeMap<String, ProviderConfig>>,
    /// 上下文压缩配置（`[compaction]`）
    pub compaction: Option<CompactionConfig>,
}

/// 上下文压缩配置（`[compaction]`），全部字段可选，缺省取内置默认
/// （enabled=true、reserve_tokens=16384、keep_recent_tokens=20000，与 pi 对齐）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionConfig {
    /// 是否启用自动压缩（手动 `/compact` 不受此开关影响）
    pub enabled: Option<bool>,
    /// 为模型响应预留的 token 数
    pub reserve_tokens: Option<u64>,
    /// 保留不压缩的近期 token 数（估算口径）
    pub keep_recent_tokens: Option<u64>,
}

impl CompactionConfig {
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

/// 单个 provider 的定义（`[providers.<名字>]`）。
///
/// `provider` 与 `base_url` 永远来自用户指定，不经由 models.dev。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// API 种类：anthropic_messages / open_ai_completions；
    /// anthropic、openai 可省略自动推断，自定义 provider 必填
    pub api: Option<ApiKind>,
    /// API base URL（优先级高于平铺的顶层 `base_url`）
    pub base_url: Option<String>,
    /// API key（优先级高于平铺的顶层 `api_key`，低于环境变量）
    pub api_key: Option<String>,
    /// 模型规格覆盖表（`[providers.<名字>.models."<模型id>"]`），全部字段可选；
    /// 缺失的字段继续向 models.dev、内置默认解析
    pub models: Option<BTreeMap<String, ModelSpec>>,
}

impl Config {
    /// 校验枚举类字段的取值，非法值硬报错并指出配置键名。
    fn validate(&self) -> Result<()> {
        if let Some(provider) = &self.provider {
            let known = BUILTIN_PROVIDERS.contains(&provider.as_str())
                || self
                    .providers
                    .as_ref()
                    .is_some_and(|providers| providers.contains_key(provider));
            if !known {
                bail!(
                    "配置项 provider 取值非法：{provider:?}\
                     （可选 anthropic / openai，或在 [providers] 表中定义）"
                );
            }
        }
        if let Some(reasoning) = &self.reasoning
            && !matches!(reasoning.as_str(), "minimal" | "low" | "medium" | "high")
        {
            bail!("配置项 reasoning 取值非法：{reasoning:?}（可选 minimal / low / medium / high）");
        }
        if let Some(providers) = &self.providers {
            for (name, provider) in providers {
                if provider.api.is_none() && infer_api(name).is_none() {
                    bail!("自定义 provider {name:?} 必须在 providers.{name}.api 中指定 API 种类");
                }
            }
        }
        Ok(())
    }
}

/// 从默认路径加载配置；文件不存在时返回 `Ok(None)`。
pub fn load() -> Result<Option<Config>> {
    let path = default_config_path()?;
    load_from(&path)
}

/// 从指定路径加载配置；不存在返回 `Ok(None)`，读取/解析/校验失败硬报错。
fn load_from(path: &Path) -> Result<Option<Config>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("读取配置文件失败：{}", path.display()));
        }
    };
    let config: Config =
        toml::from_str(&text).with_context(|| format!("解析配置文件失败：{}", path.display()))?;
    config
        .validate()
        .with_context(|| format!("校验配置文件失败：{}", path.display()))?;
    Ok(Some(config))
}

/// 默认配置路径：`$XDG_CONFIG_HOME/nomic/config.toml`，fallback `~/.config/nomic/config.toml`。
///
/// 手写解析 XDG，不引入 `dirs` 依赖（与 `nomic-session` 的 `default_db_path` 一致）；
/// 无 `HOME` 时返回 io 错误。
fn default_config_path() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("nomic").join("config.toml"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve default config path: neither XDG_CONFIG_HOME nor HOME is set",
        )
    })?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("nomic")
        .join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).expect("write config");
        (dir, path)
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nonexistent.toml");
        assert!(load_from(&path).expect("load").is_none());
    }

    #[test]
    fn parses_full_config() {
        let (_dir, path) = write_temp(
            r#"
provider = "anthropic"
model = "claude-sonnet-4-5"
base_url = "https://api.anthropic.com"
api_key = "sk-ant-test"
reasoning = "low"
temperature = 0.7
max_tokens = 8192
append_system = "Always reply in Chinese."
"#,
        );
        let config = load_from(&path).expect("load").expect("some");
        assert_eq!(config.provider.as_deref(), Some("anthropic"));
        assert_eq!(config.model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(config.reasoning.as_deref(), Some("low"));
        assert_eq!(config.temperature, Some(0.7));
        assert_eq!(config.max_tokens, Some(8192));
        assert_eq!(
            config.append_system.as_deref(),
            Some("Always reply in Chinese.")
        );
    }

    #[test]
    fn empty_config_is_default() {
        let (_dir, path) = write_temp("");
        let config = load_from(&path).expect("load").expect("some");
        assert!(config.provider.is_none());
        assert!(config.model.is_none());
    }

    #[test]
    fn parses_prompts_list() {
        let (_dir, path) =
            write_temp("prompts = [\"prompts/review.md\", \"/opt/extra-prompts\"]\n");
        let config = load_from(&path).expect("load").expect("some");
        assert_eq!(
            config.prompts.expect("prompts"),
            vec![
                PathBuf::from("prompts/review.md"),
                PathBuf::from("/opt/extra-prompts")
            ]
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        let (_dir, path) = write_temp("providr = \"anthropic\"\n");
        let error = load_from(&path).expect_err("unknown field must fail");
        assert!(format!("{error:#}").contains("解析配置文件失败"));
    }

    #[test]
    fn parses_compaction_section_and_merges_defaults() {
        let (_dir, path) = write_temp("[compaction]\nreserve_tokens = 8192\n");
        let config = load_from(&path).expect("load").expect("some");
        let settings = config.compaction.as_ref().expect("compaction").settings();
        assert!(settings.enabled, "未指定时取默认 true");
        assert_eq!(settings.reserve_tokens, 8192);
        assert_eq!(
            settings.keep_recent_tokens,
            nomic_core::CompactionSettings::default().keep_recent_tokens
        );
    }

    #[test]
    fn unknown_field_in_compaction_is_rejected() {
        let (_dir, path) = write_temp("[compaction]\nreserve_token = 1\n");
        let error = load_from(&path).expect_err("unknown field must fail");
        assert!(format!("{error:#}").contains("解析配置文件失败"));
    }

    #[test]
    fn invalid_provider_is_rejected() {
        let (_dir, path) = write_temp("provider = \"gemini\"\n");
        let error = load_from(&path).expect_err("invalid provider must fail");
        assert!(format!("{error:#}").contains("provider"));
    }

    #[test]
    fn invalid_reasoning_is_rejected() {
        let (_dir, path) = write_temp("reasoning = \"extreme\"\n");
        let error = load_from(&path).expect_err("invalid reasoning must fail");
        assert!(format!("{error:#}").contains("reasoning"));
    }

    // ── [providers.*] 嵌套 models 配置 ─────────────────────────────────────

    #[test]
    fn parses_providers_with_nested_model_specs() {
        let (_dir, path) = write_temp(
            r#"
provider = "deepseek"
model = "deepseek-chat"

[providers.anthropic]
base_url = "https://api.anthropic.com"
api_key = "sk-ant-test"

[providers.anthropic.models."claude-sonnet-4-5"]
name = "Claude Sonnet 4.5"
reasoning = true
context_window = 200000
max_tokens = 64000
cost_input = 3.0
cost_output = 15.0
cost_cache_read = 0.3
cost_cache_write = 3.75

[providers.deepseek]
api = "open_ai_completions"
base_url = "https://api.deepseek.com/v1"

[providers.deepseek.models."deepseek-chat"]
max_tokens = 8192
"#,
        );
        let config = load_from(&path).expect("load").expect("some");
        let providers = config.providers.as_ref().expect("providers");

        let anthropic = providers.get("anthropic").expect("anthropic");
        assert_eq!(anthropic.api, None, "内置 provider 的 api 可省略");
        assert_eq!(
            anthropic.base_url.as_deref(),
            Some("https://api.anthropic.com")
        );
        let spec = anthropic
            .models
            .as_ref()
            .and_then(|m| m.get("claude-sonnet-4-5"))
            .expect("model spec");
        assert_eq!(spec.context_window, Some(200_000));
        assert_eq!(spec.cost_cache_write, Some(3.75));
        assert!(spec.is_complete());

        let deepseek = providers.get("deepseek").expect("deepseek");
        assert_eq!(deepseek.api, Some(ApiKind::OpenAiCompletions));
        let spec = deepseek
            .models
            .as_ref()
            .and_then(|m| m.get("deepseek-chat"))
            .expect("model spec");
        assert_eq!(spec.max_tokens, Some(8192));
        assert!(!spec.is_complete(), "只写部分字段时不完整");
    }

    #[test]
    fn custom_provider_without_api_is_rejected() {
        let (_dir, path) = write_temp(
            r#"
[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
"#,
        );
        let error = load_from(&path).expect_err("custom provider without api must fail");
        assert!(format!("{error:#}").contains("deepseek"));
        assert!(format!("{error:#}").contains("api"));
    }

    #[test]
    fn provider_selector_may_reference_defined_custom_provider() {
        let (_dir, path) = write_temp(
            r#"
provider = "deepseek"

[providers.deepseek]
api = "open_ai_completions"
base_url = "https://api.deepseek.com/v1"
"#,
        );
        let config = load_from(&path).expect("load").expect("some");
        assert_eq!(config.provider.as_deref(), Some("deepseek"));
    }

    #[test]
    fn unknown_field_in_model_spec_is_rejected() {
        let (_dir, path) = write_temp(
            r#"
[providers.openai.models."gpt-5.2"]
contex_window = 400000
"#,
        );
        let error = load_from(&path).expect_err("typo in model spec must fail");
        assert!(format!("{error:#}").contains("解析配置文件失败"));
    }

    #[test]
    fn repo_example_config_parses_and_validates() {
        // 仓库根目录的 config.example.toml 必须与配置 schema 同步
        let text = include_str!("../../../config.example.toml");
        let config: Config = toml::from_str(text).expect("示例配置必须可解析");
        config.validate().expect("示例配置必须合法");
    }
}
