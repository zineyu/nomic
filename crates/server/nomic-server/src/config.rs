//! 运行配置：环境变量与 `~/.config/nomic/config.toml`。
//!
//! 优先级（高到低）：命令行 flag > 环境变量 > 配置文件 > 默认值。
//! provider 完全未出现时视为未配置（server 仍可管理会话）；一旦出现
//! 任一 provider 字段，缺失 api_key 或 model 是显式错误。

use std::path::{Path, PathBuf};

use nomic_ai::{ApiKey, ProviderConfig, ProviderConfigError};
use serde::Deserialize;

/// 环境变量来源；生产用 [`ProcessEnv`]，测试注入固定映射。
pub trait EnvSource {
    /// 读取环境变量；未设置或非 UTF-8 返回 `None`。
    fn get(&self, key: &str) -> Option<String>;
}

/// 进程环境。
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// 环境变量名。
mod env_names {
    /// provider API key。
    pub const API_KEY: &str = "NOMIC_API_KEY";
    /// provider 端点基址。
    pub const BASE_URL: &str = "NOMIC_BASE_URL";
    /// provider 模型名。
    pub const MODEL: &str = "NOMIC_MODEL";
    /// 数据目录。
    pub const DATA_DIR: &str = "NOMIC_DATA_DIR";
    /// socket 路径。
    pub const SOCKET: &str = "NOMIC_SOCKET";
    /// 存储文件路径。
    pub const STORAGE: &str = "NOMIC_STORAGE";
}

/// 生效的运行配置。
#[derive(Debug, Clone)]
pub struct Config {
    data_dir: PathBuf,
    socket: PathBuf,
    storage: PathBuf,
    provider: Option<ProviderConfig>,
    /// socket 路径来自显式配置（环境变量或文件），不随 `data_dir` 派生。
    socket_configured: bool,
    /// 存储路径来自显式配置（环境变量或文件），不随 `data_dir` 派生。
    storage_configured: bool,
}

impl Config {
    /// 读取配置文件（不存在则视为空配置）并解析。
    pub fn load(env: &impl EnvSource, config_path: &Path) -> Result<Self, ConfigError> {
        let file = match std::fs::read_to_string(config_path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(ConfigError::Read {
                    path: config_path.to_path_buf(),
                    source,
                });
            }
        };
        Self::resolve(env, file.as_deref(), config_path)
    }

    /// 由环境变量与配置文件内容解析配置。
    pub fn resolve(
        env: &impl EnvSource,
        file: Option<&str>,
        config_path: &Path,
    ) -> Result<Self, ConfigError> {
        let file: FileConfig = match file {
            Some(text) => match toml::from_str(text) {
                Ok(file) => file,
                // provider.api_key 是明文密钥；解析错误不得回显文件正文
                Err(mut source) => {
                    let location = source.span().map_or_else(
                        || "位置未知".to_owned(),
                        |span| line_column(text, span.start),
                    );
                    source.set_input(None);
                    return Err(ConfigError::Parse {
                        path: config_path.to_path_buf(),
                        location,
                        source: Box::new(source),
                    });
                }
            },
            None => FileConfig::default(),
        };
        let file_provider = file.provider.unwrap_or_default();
        let provider = resolve_provider(
            pick(env, env_names::BASE_URL, file_provider.base_url),
            pick(env, env_names::API_KEY, file_provider.api_key),
            pick(env, env_names::MODEL, file_provider.model),
            config_path,
        )?;

        let home = home(env);
        let data_dir = pick(env, env_names::DATA_DIR, file.data_dir)
            .map_or_else(|| default_data_dir(env), |raw| expand_tilde(&raw, &home));
        let socket_configured = pick(env, env_names::SOCKET, file.socket);
        let storage_configured = pick(env, env_names::STORAGE, file.storage);
        let socket_explicit = socket_configured.as_ref();
        let socket = socket_explicit.map_or_else(
            || data_dir.join("nomic.sock"),
            |raw| expand_tilde(raw, &home),
        );
        let storage_explicit = storage_configured.as_ref();
        let storage = storage_explicit
            .map_or_else(|| data_dir.join("nomic.db"), |raw| expand_tilde(raw, &home));

        Ok(Self {
            data_dir,
            socket,
            storage,
            provider,
            socket_configured: socket_explicit.is_some(),
            storage_configured: storage_explicit.is_some(),
        })
    }

    /// 应用命令行覆盖：`data_dir` 重定位默认派生的 socket 与存储，
    /// 显式配置的存储路径与 `--socket` 保持不变。
    #[must_use]
    pub fn apply_cli(mut self, data_dir: Option<PathBuf>, socket: Option<PathBuf>) -> Self {
        if let Some(dir) = data_dir {
            if !self.storage_configured {
                self.storage = dir.join("nomic.db");
            }
            if !self.socket_configured && socket.is_none() {
                self.socket = dir.join("nomic.sock");
            }
            self.data_dir = dir;
        }
        if let Some(socket) = socket {
            self.socket = socket;
        }
        self
    }

    /// 数据目录。
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Unix socket 路径。
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// SQLite 存储文件路径。
    #[must_use]
    pub fn storage(&self) -> &Path {
        &self.storage
    }

    /// provider 配置；`None` 表示未配置。
    #[must_use]
    pub const fn provider(&self) -> Option<&ProviderConfig> {
        self.provider.as_ref()
    }
}

/// 解析 provider：未出现任何字段视为未配置，部分配置是显式错误。
fn resolve_provider(
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    config_path: &Path,
) -> Result<Option<ProviderConfig>, ConfigError> {
    if base_url.is_none() && api_key.is_none() && model.is_none() {
        return Ok(None);
    }
    let api_key = api_key.ok_or_else(|| ConfigError::MissingApiKey {
        path: config_path.to_path_buf(),
    })?;
    let api_key = ApiKey::parse(api_key).map_err(|source| match source {
        ProviderConfigError::MissingApiKey => ConfigError::MissingApiKey {
            path: config_path.to_path_buf(),
        },
        source => ConfigError::Provider {
            path: config_path.to_path_buf(),
            source,
        },
    })?;
    let model = model.ok_or_else(|| ConfigError::Provider {
        path: config_path.to_path_buf(),
        source: ProviderConfigError::MissingModel,
    })?;
    ProviderConfig::new(base_url, api_key, model)
        .map(Some)
        .map_err(|source| ConfigError::Provider {
            path: config_path.to_path_buf(),
            source,
        })
}

/// 环境变量优先于文件值；空值（含纯空白）视为未设置。
fn pick(env: &impl EnvSource, key: &str, file: Option<String>) -> Option<String> {
    env.get(key)
        .filter(|value| !value.trim().is_empty())
        .or(file)
        .filter(|value| !value.trim().is_empty())
}

/// 字节偏移对应的行号与列号（均从 1 开始）。
fn line_column(text: &str, offset: usize) -> String {
    let mut line = 1_usize;
    let mut column = 1_usize;
    for (index, character) in text.char_indices() {
        if index >= offset {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    format!("第 {line} 行第 {column} 列")
}

/// 展开路径开头的 `~/`。
fn expand_tilde(raw: &str, home: &Path) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(raw),
    }
}

/// `HOME` 目录；未设置时 fast-fail。
fn home(env: &impl EnvSource) -> PathBuf {
    PathBuf::from(env.get("HOME").expect("HOME 环境变量未设置"))
}

/// 默认数据目录：`~/.local/share/nomic`。
pub fn default_data_dir(env: &impl EnvSource) -> PathBuf {
    home(env).join(".local/share/nomic")
}

/// 默认配置文件：`$XDG_CONFIG_HOME/nomic/config.toml`，否则
/// `~/.config/nomic/config.toml`。
pub fn default_config_path(env: &impl EnvSource) -> PathBuf {
    match env.get("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("nomic/config.toml"),
        None => home(env).join(".config/nomic/config.toml"),
    }
}

/// 配置文件模型。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    data_dir: Option<String>,
    socket: Option<String>,
    storage: Option<String>,
    provider: Option<FileProvider>,
}

/// provider 配置段。
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileProvider {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
}

/// 配置加载失败。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// 配置文件不可读。
    #[error("读取配置失败 {path}: {source}")]
    Read {
        /// 配置文件路径。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },
    /// 配置文件不是合法 TOML 或含未知字段。
    #[error("解析配置失败 {path}（{location}）：{source}")]
    Parse {
        /// 配置文件路径。
        path: PathBuf,
        /// 出错位置（行/列）。
        location: String,
        /// TOML 解析错误；正文已被剥离，不回显密钥。
        #[source]
        source: Box<toml::de::Error>,
    },
    /// provider 部分配置但缺少 api_key。
    #[error("provider 缺少 api_key：设置 {key} 或在 {path} 写入 provider.api_key", key = env_names::API_KEY)]
    MissingApiKey {
        /// 配置文件路径。
        path: PathBuf,
    },
    /// provider 字段非法。
    #[error("provider 配置无效（{path}）：{source}")]
    Provider {
        /// 配置文件路径。
        path: PathBuf,
        /// 字段错误。
        #[source]
        source: ProviderConfigError,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Debug, Default)]
    struct MapEnv(BTreeMap<String, String>);

    impl MapEnv {
        fn new(pairs: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
            Self(
                pairs
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect(),
            )
        }
    }

    impl EnvSource for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    fn home_env() -> MapEnv {
        MapEnv::new([("HOME", "/home/tester")])
    }

    fn path() -> PathBuf {
        PathBuf::from("/home/tester/.config/nomic/config.toml")
    }

    #[test]
    fn 无配置时使用默认路径且_provider_未配置() {
        let config = Config::resolve(&home_env(), None, &path()).unwrap();
        assert_eq!(
            config.data_dir(),
            Path::new("/home/tester/.local/share/nomic")
        );
        assert_eq!(
            config.socket(),
            Path::new("/home/tester/.local/share/nomic/nomic.sock")
        );
        assert_eq!(
            config.storage(),
            Path::new("/home/tester/.local/share/nomic/nomic.db")
        );
        assert!(config.provider().is_none());
    }

    #[test]
    fn 环境变量优先于配置文件() {
        let env = MapEnv::new([
            ("HOME", "/home/tester"),
            ("NOMIC_MODEL", "env-model"),
            ("NOMIC_SOCKET", "/tmp/env.sock"),
        ]);
        let file = r#"
            socket = "/tmp/file.sock"
            [provider]
            api_key = "file-key"
            model = "file-model"
        "#;
        let config = Config::resolve(&env, Some(file), &path()).unwrap();
        assert_eq!(config.socket(), Path::new("/tmp/env.sock"));
        let provider = config.provider().unwrap();
        assert_eq!(provider.model(), "env-model");
    }

    #[test]
    fn 文件里的_provider_与路径生效() {
        let file = r#"
            data_dir = "~/data"
            [provider]
            base_url = "https://example.test/v1"
            api_key = "file-key"
            model = "file-model"
        "#;
        let config = Config::resolve(&home_env(), Some(file), &path()).unwrap();
        assert_eq!(config.data_dir(), Path::new("/home/tester/data"));
        assert_eq!(config.socket(), Path::new("/home/tester/data/nomic.sock"));
        assert_eq!(config.storage(), Path::new("/home/tester/data/nomic.db"));
        let provider = config.provider().unwrap();
        assert_eq!(provider.base_url(), Some("https://example.test/v1"));
        assert_eq!(provider.model(), "file-model");
        assert!(
            !format!("{provider:?}").contains("file-key"),
            "密钥不得出现在 Debug 中"
        );
    }

    #[test]
    fn 部分_provider_缺_api_key_报错可读() {
        let file = r#"
            [provider]
            model = "file-model"
        "#;
        let error = Config::resolve(&home_env(), Some(file), &path()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("NOMIC_API_KEY"), "实际: {message}");
        assert!(message.contains("provider.api_key"), "实际: {message}");
    }

    #[test]
    fn provider_缺_model_报错() {
        let file = r#"
            [provider]
            api_key = "file-key"
        "#;
        let error = Config::resolve(&home_env(), Some(file), &path()).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::Provider {
                source: ProviderConfigError::MissingModel,
                ..
            }
        ));
    }

    #[test]
    fn 非法_toml_与未知字段报错() {
        let error = Config::resolve(&home_env(), Some("socket = ["), &path()).unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }));
        let error = Config::resolve(&home_env(), Some("unknown = 1"), &path()).unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }));
    }

    #[test]
    fn 命令行覆盖_派生_socket_与存储() {
        let config = Config::resolve(&home_env(), None, &path())
            .unwrap()
            .apply_cli(Some(PathBuf::from("/tmp/dir")), None);
        assert_eq!(config.data_dir(), Path::new("/tmp/dir"));
        assert_eq!(config.socket(), Path::new("/tmp/dir/nomic.sock"));
        assert_eq!(config.storage(), Path::new("/tmp/dir/nomic.db"));
    }

    #[test]
    fn 显式存储与_socket_不被_data_dir_覆盖() {
        let file = r#"
            storage = "/var/lib/nomic.db"
            socket = "/var/lib/nomic.sock"
        "#;
        let config = Config::resolve(&home_env(), Some(file), &path())
            .unwrap()
            .apply_cli(
                Some(PathBuf::from("/tmp/dir")),
                Some(PathBuf::from("/tmp/other.sock")),
            );
        assert_eq!(config.data_dir(), Path::new("/tmp/dir"));
        assert_eq!(config.storage(), Path::new("/var/lib/nomic.db"));
        assert_eq!(config.socket(), Path::new("/tmp/other.sock"));
    }

    #[test]
    fn 环境变量可覆盖存储路径() {
        let env = MapEnv::new([
            ("HOME", "/home/tester"),
            ("NOMIC_STORAGE", "/tmp/custom.db"),
        ]);
        let config = Config::resolve(&env, None, &path())
            .unwrap()
            .apply_cli(Some(PathBuf::from("/tmp/dir")), None);
        assert_eq!(config.storage(), Path::new("/tmp/custom.db"));
        assert_eq!(config.socket(), Path::new("/tmp/dir/nomic.sock"));
    }

    #[test]
    fn 配置路径遵循_xdg_与_home() {
        assert_eq!(
            default_config_path(&home_env()),
            Path::new("/home/tester/.config/nomic/config.toml")
        );
        let env = MapEnv::new([("HOME", "/home/tester"), ("XDG_CONFIG_HOME", "/xdg")]);
        assert_eq!(
            default_config_path(&env),
            Path::new("/xdg/nomic/config.toml")
        );
    }
}
