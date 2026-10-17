//! S5（#40）配置验收：优先级、CLI 覆盖、可读错误、密钥不进错误文本。
//!
//! 全部经公开接口 `Config::resolve` / `Config::apply_cli` 驱动，
//! 不触网、不落盘。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nomic_server::config::{Config, ConfigError, EnvSource};

/// 固定映射的环境变量来源。
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
fn 环境变量_socket_不被_cli_data_dir_覆盖() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_SOCKET", "/tmp/env.sock")]);
    let config = Config::resolve(&env, None, &path())
        .unwrap()
        .apply_cli(Some(PathBuf::from("/tmp/dir")), None);
    assert_eq!(
        config.socket(),
        Path::new("/tmp/env.sock"),
        "--data-dir 不得覆盖显式配置的 NOMIC_SOCKET"
    );
    assert_eq!(config.data_dir(), Path::new("/tmp/dir"));
    assert_eq!(config.storage(), Path::new("/tmp/dir/nomic.db"));
}

#[test]
fn 配置文件_socket_不被_cli_data_dir_覆盖() {
    let file = r#"
        socket = "/var/lib/nomic.sock"
    "#;
    let config = Config::resolve(&home_env(), Some(file), &path())
        .unwrap()
        .apply_cli(Some(PathBuf::from("/tmp/dir")), None);
    assert_eq!(
        config.socket(),
        Path::new("/var/lib/nomic.sock"),
        "--data-dir 不得覆盖配置文件中的 socket"
    );
}

#[test]
fn cli_socket_优先于_data_dir() {
    let config = Config::resolve(&home_env(), None, &path())
        .unwrap()
        .apply_cli(
            Some(PathBuf::from("/tmp/dir")),
            Some(PathBuf::from("/tmp/explicit.sock")),
        );
    assert_eq!(config.socket(), Path::new("/tmp/explicit.sock"));
    assert_eq!(config.storage(), Path::new("/tmp/dir/nomic.db"));
}

#[test]
fn 环境变量_socket_生效且不与存储交错() {
    let env = MapEnv::new([
        ("HOME", "/home/tester"),
        ("NOMIC_SOCKET", "/tmp/env.sock"),
        ("NOMIC_STORAGE", "/tmp/env.db"),
    ]);
    let config = Config::resolve(&env, None, &path())
        .unwrap()
        .apply_cli(Some(PathBuf::from("/tmp/dir")), None);
    assert_eq!(config.socket(), Path::new("/tmp/env.sock"));
    assert_eq!(config.storage(), Path::new("/tmp/env.db"));
}

#[test]
fn 配置文件语法错误_不把密钥正文带进错误消息() {
    let file = "[provider]\napi_key = sk-super-secret-value\nmodel = \"m\"\n";
    let error = Config::resolve(&home_env(), Some(file), &path()).unwrap_err();
    let message = error.to_string();
    assert!(
        !message.contains("sk-super-secret-value"),
        "解析错误泄露密钥正文: {message}"
    );
    assert!(message.contains("第 2 行"), "错误消息应给出位置: {message}");
}

#[test]
fn 空_env_变量视为未设置_回退文件值() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_API_KEY", "")]);
    let file = r#"
        [provider]
        api_key = "file-key"
        model = "file-model"
    "#;
    let provider = Config::resolve(&env, Some(file), &path())
        .unwrap()
        .provider()
        .cloned()
        .expect("空 env 视为未设置，回退文件值");
    assert_eq!(provider.model(), "file-model");
}

#[test]
fn 纯空白_api_key_报错也含环境变量名() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_API_KEY", "   ")]);
    let file = r#"
        [provider]
        model = "file-model"
    "#;
    let error = Config::resolve(&env, Some(file), &path()).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("NOMIC_API_KEY"), "实际: {message}");
}

#[test]
fn 缺_api_key_错误含环境变量名与文件路径() {
    let file = r#"
        [provider]
        model = "file-model"
    "#;
    let error = Config::resolve(&home_env(), Some(file), &path()).unwrap_err();
    assert!(matches!(error, ConfigError::MissingApiKey { .. }));
    let message = error.to_string();
    assert!(message.contains("NOMIC_API_KEY"), "实际: {message}");
    assert!(
        message.contains("/home/tester/.config/nomic/config.toml"),
        "实际: {message}"
    );
}

#[test]
fn 只有_model_或只有_base_url_都显式失败() {
    for file in [
        r#"
            [provider]
            model = "m"
        "#,
        r#"
            [provider]
            base_url = "https://example.test/v1"
        "#,
    ] {
        let error = Config::resolve(&home_env(), Some(file), &path()).unwrap_err();
        assert!(error.to_string().contains("NOMIC_API_KEY"), "实际: {error}");
    }
}

#[test]
fn provider_完全未出现时未配置而非报错() {
    let config = Config::resolve(&home_env(), Some(""), &path()).unwrap();
    assert!(config.provider().is_none());
}

#[test]
fn 环境变量逐字段优先于_toml() {
    let env = MapEnv::new([
        ("HOME", "/home/tester"),
        ("NOMIC_BASE_URL", "https://env.test/v1"),
        ("NOMIC_MODEL", "env-model"),
    ]);
    let file = r#"
        [provider]
        base_url = "https://file.test/v1"
        api_key = "file-key"
        model = "file-model"
    "#;
    let provider = Config::resolve(&env, Some(file), &path())
        .unwrap()
        .provider()
        .cloned()
        .unwrap();
    assert_eq!(provider.base_url(), Some("https://env.test/v1"));
    assert_eq!(provider.model(), "env-model");
}

#[test]
fn tilde_在_data_dir_与_socket_上展开() {
    let file = r#"
        data_dir = "~/nomic-data"
        socket = "~/nomic-data/custom.sock"
    "#;
    let config = Config::resolve(&home_env(), Some(file), &path()).unwrap();
    assert_eq!(config.data_dir(), Path::new("/home/tester/nomic-data"));
    assert_eq!(
        config.socket(),
        Path::new("/home/tester/nomic-data/custom.sock")
    );
}
