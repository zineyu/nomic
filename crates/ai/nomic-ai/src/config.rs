//! provider 配置：密钥脱敏，构造即合法。

use std::fmt;

/// provider API key。
///
/// `Debug` 输出脱敏；不实现 `Serialize`，密钥因此无法进入会话存储
/// 或日志。原文只在构造适配器时取出一次。
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// 解析密钥：空值（含纯空白）是显式错误；首尾空白被剔除。
    pub fn parse(value: impl Into<String>) -> Result<Self, ProviderConfigError> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty() {
            return Err(ProviderConfigError::MissingApiKey);
        }
        Ok(Self(value.to_owned()))
    }

    /// 密钥原文；仅供构造 provider 客户端使用。
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}

/// provider 配置：OpenAI 兼容端点、密钥与模型。
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    base_url: Option<String>,
    api_key: ApiKey,
    model: String,
}

impl ProviderConfig {
    /// 组装配置；空模型与空 `base_url` 是显式错误。
    pub fn new(
        base_url: Option<String>,
        api_key: ApiKey,
        model: impl Into<String>,
    ) -> Result<Self, ProviderConfigError> {
        let model = model.into();
        let model = model.trim();
        if model.is_empty() {
            return Err(ProviderConfigError::MissingModel);
        }
        let base_url = match base_url {
            Some(url) if url.trim().is_empty() => {
                return Err(ProviderConfigError::EmptyBaseUrl);
            }
            Some(url) => Some(url.trim().to_owned()),
            None => None,
        };
        Ok(Self {
            base_url,
            api_key,
            model: model.to_owned(),
        })
    }

    /// 端点基址；`None` 表示使用 provider 默认地址。
    #[must_use]
    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// 模型名。
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 密钥（脱敏类型）。
    #[must_use]
    pub const fn api_key(&self) -> &ApiKey {
        &self.api_key
    }
}

/// provider 配置的非法状态。
#[derive(Debug, thiserror::Error)]
pub enum ProviderConfigError {
    /// 缺少密钥。
    #[error("缺少 api_key")]
    MissingApiKey,
    /// 缺少模型名。
    #[error("缺少 model")]
    MissingModel,
    /// `base_url` 是空串。
    #[error("base_url 为空串；删除该项以使用 provider 默认地址")]
    EmptyBaseUrl,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 密钥_debug_脱敏() {
        let key = ApiKey::parse("sk-super-secret").unwrap();
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("sk-super-secret"), "实际: {rendered}");
        assert!(rendered.contains("redacted"));
        let config = ProviderConfig::new(None, key, "gpt-test").expect("合法配置不应失败");
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("sk-super-secret"), "实际: {rendered}");
    }

    #[test]
    fn 首尾空白被剔除() {
        let key = ApiKey::parse("  sk-test  ").unwrap();
        assert_eq!(key.expose(), "sk-test");
        let config = ProviderConfig::new(
            Some("  https://example.test/v1 ".into()),
            key,
            "  gpt-test ",
        )
        .unwrap();
        assert_eq!(config.base_url(), Some("https://example.test/v1"));
        assert_eq!(config.model(), "gpt-test");
    }

    #[test]
    fn 空密钥与空模型是显式错误() {
        assert!(matches!(
            ApiKey::parse("   "),
            Err(ProviderConfigError::MissingApiKey)
        ));
        let key = ApiKey::parse("k").unwrap();
        assert!(matches!(
            ProviderConfig::new(None, key.clone(), ""),
            Err(ProviderConfigError::MissingModel)
        ));
        assert!(matches!(
            ProviderConfig::new(Some(String::new()), key, "m"),
            Err(ProviderConfigError::EmptyBaseUrl)
        ));
    }
}
