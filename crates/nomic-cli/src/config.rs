//! 用户级配置文件加载：路径解析、TOML 反序列化与字段校验。
//!
//! 配置文件为可选：文件不存在时视为无配置；存在但读取/解析/校验失败时硬报错
//! （用户显式写了配置，静默降级会掩盖拼写错误等问题）。整体优先级为
//! CLI 参数 > 环境变量 > 配置文件 > 内置默认，本模块只负责「配置文件」这一层，
//! 分层合并在 `bootstrap` 完成。

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// 用户级配置（`config.toml` 的反序列化目标）。
///
/// 全部字段可选：缺省的字段继续向上取环境变量与内置默认；
/// `deny_unknown_fields` 让未知键硬报错，避免拼写错误被静默忽略。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// provider：anthropic 或 openai（兼容端点）
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
}

impl Config {
    /// 校验枚举类字段的取值，非法值硬报错并指出配置键名。
    fn validate(&self) -> Result<()> {
        if let Some(provider) = &self.provider {
            if !matches!(provider.as_str(), "anthropic" | "openai") {
                bail!("配置项 provider 取值非法：{provider:?}（可选 anthropic / openai）");
            }
        }
        if let Some(reasoning) = &self.reasoning {
            if !matches!(reasoning.as_str(), "minimal" | "low" | "medium" | "high") {
                bail!(
                    "配置项 reasoning 取值非法：{reasoning:?}（可选 minimal / low / medium / high）"
                );
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
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("nomic").join("config.toml"));
        }
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
    fn unknown_field_is_rejected() {
        let (_dir, path) = write_temp("providr = \"anthropic\"\n");
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
}
