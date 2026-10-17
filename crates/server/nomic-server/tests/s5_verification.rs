//! S5（#40）配置验收独立探针：路径优先级交叉与密钥泄露面。
//!
//! 从规格重新推导，覆盖实现者测试未覆盖的边界：CLI/环境/文件三级
//! 路径优先级交叉、TOML 解析错误正文泄露的多形态触发、配置 Debug
//! 脱敏。全部经公开接口驱动，不触网、不落盘。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nomic_server::config::{Config, ConfigError, EnvSource};

/// 探针密钥哨兵：任何出现即泄露。
const SECRET: &str = "sk-probe-super-secret-9c1f";

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
fn cli_socket_高于环境变量与文件() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_SOCKET", "/env.sock")]);
    let file = r#"socket = "/file.sock""#;
    let config = Config::resolve(&env, Some(file), &path())
        .unwrap()
        .apply_cli(
            Some(PathBuf::from("/data")),
            Some(PathBuf::from("/cli.sock")),
        );
    assert_eq!(config.socket(), Path::new("/cli.sock"));
    assert_eq!(config.data_dir(), Path::new("/data"));
}

#[test]
fn 环境变量_socket_高于文件的_data_dir_派生() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_SOCKET", "/env.sock")]);
    let file = r#"data_dir = "/file-data""#;
    let config = Config::resolve(&env, Some(file), &path()).unwrap();
    assert_eq!(config.socket(), Path::new("/env.sock"));
    assert_eq!(config.data_dir(), Path::new("/file-data"));
    assert_eq!(config.storage(), Path::new("/file-data/nomic.db"));
}

#[test]
fn 环境变量_data_dir_不覆盖文件的_socket() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_DATA_DIR", "/env-data")]);
    let file = r#"socket = "/file.sock""#;
    let config = Config::resolve(&env, Some(file), &path()).unwrap();
    assert_eq!(config.data_dir(), Path::new("/env-data"));
    assert_eq!(config.socket(), Path::new("/file.sock"));
    assert_eq!(config.storage(), Path::new("/env-data/nomic.db"));
}

#[test]
fn 空白_socket_环境变量视为未设置_由_data_dir_派生() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_SOCKET", "   ")]);
    let config = Config::resolve(&env, None, &path())
        .unwrap()
        .apply_cli(Some(PathBuf::from("/data")), None);
    assert_eq!(config.socket(), Path::new("/data/nomic.sock"));
}

#[test]
fn 环境变量的_storage_不被_data_dir_覆盖() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_STORAGE", "/env.db")]);
    let config = Config::resolve(&env, None, &path())
        .unwrap()
        .apply_cli(Some(PathBuf::from("/data")), None);
    assert_eq!(config.storage(), Path::new("/env.db"));
    assert_eq!(config.socket(), Path::new("/data/nomic.sock"));
}

#[test]
fn toml_解析错误不回显密钥正文_多形态() {
    let bodies = [
        format!("api_key = {SECRET}"),
        format!("api_key = \"{SECRET}"),
        format!("api_key = '{SECRET}"),
        format!("api_key = \"{SECRET}\" \"extra\""),
        format!("api_key = \"{SECRET}\"\napi_key = \"other\""),
        format!("[provider]\napi_key = \"{SECRET}\"\nmodel ="),
        format!("[provider]\napi_key = \"{SECRET}\"\nmodel = 1 2 3"),
        format!("unknown = \"{SECRET}\""),
        format!("api_key = [\"{SECRET}\""),
        format!("api_key = @{SECRET}@"),
        format!("[provider]\napi_key = {{ value = \"{SECRET}\""),
    ];
    for body in bodies {
        let message = match Config::resolve(&home_env(), Some(&body), &path()) {
            Ok(_) => panic!("非法 TOML 必须失败: {body}"),
            Err(error) => error.to_string(),
        };
        assert!(
            !message.contains(SECRET),
            "密钥正文进入错误消息: {message}\n输入: {body}"
        );
    }
}

#[test]
fn 配置_debug_不含密钥() {
    let file = format!(
        "[provider]\nbase_url = \"https://example.test/v1\"\napi_key = \"{SECRET}\"\nmodel = \"m\""
    );
    let config = Config::resolve(&home_env(), Some(&file), &path()).unwrap();
    let rendered = format!("{config:?}");
    assert!(
        !rendered.contains(SECRET),
        "Config Debug 泄露密钥: {rendered}"
    );
}

#[test]
fn 纯空白_api_key_环境变量报错含变量名与路径() {
    let env = MapEnv::new([
        ("HOME", "/home/tester"),
        ("NOMIC_API_KEY", "  "),
        ("NOMIC_MODEL", "m"),
    ]);
    let error = Config::resolve(&env, None, &path()).unwrap_err();
    assert!(matches!(error, ConfigError::MissingApiKey { .. }));
    let message = error.to_string();
    assert!(message.contains("NOMIC_API_KEY"), "实际: {message}");
    assert!(
        message.contains("/home/tester/.config/nomic/config.toml"),
        "实际: {message}"
    );
}

#[test]
fn 纯空白_api_key_环境变量回退文件值() {
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_API_KEY", "   ")]);
    let file = format!("[provider]\napi_key = \"{SECRET}\"\nmodel = \"m\"");
    let provider = Config::resolve(&env, Some(&file), &path())
        .unwrap()
        .provider()
        .cloned()
        .expect("空白 env 视为未设置，回退文件值");
    assert_eq!(provider.model(), "m");
}

#[test]
fn 环境_variable_base_url_不能为空串覆盖文件() {
    // 空 env 的 base_url 视为未设置；文件 base_url 生效
    let env = MapEnv::new([("HOME", "/home/tester"), ("NOMIC_BASE_URL", "  ")]);
    let file = format!(
        "[provider]\nbase_url = \"https://file.test/v1\"\napi_key = \"{SECRET}\"\nmodel = \"m\""
    );
    let provider = Config::resolve(&env, Some(&file), &path())
        .unwrap()
        .provider()
        .cloned()
        .unwrap();
    assert_eq!(provider.base_url(), Some("https://file.test/v1"));
}
