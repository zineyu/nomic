//! nix 纯净环境的集成测试（ADR-0041）。需要真实 `nix`（含 flakes）与
//! 网络（首次拉取 nixpkgs 与包），`nix` 不在 PATH 时整体跳过。

use nomic_core::{AgentTool, ToolResult, ToolUpdateCallback};
use nomic_tools::BashTool;
use nomic_tools::nix_env::{self, NixEnvCache};
use tokio_util::sync::CancellationToken;

fn nix_available() -> bool {
    std::process::Command::new("nix")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

macro_rules! skip_without_nix {
    () => {
        if !nix_available() {
            eprintln!("SKIP: nix not on PATH");
            return;
        }
    };
}

fn noop_update() -> ToolUpdateCallback {
    Box::new(|_| {})
}

fn text_of(result: &ToolResult) -> &str {
    let [nomic_ai::UserContent::Text(text)] = &result.content[..] else {
        panic!("expected text result");
    };
    &text.text
}

/// 默认模板可解析：env 含 nix store 的 PATH、bash 可解析、HOME 已补齐。
#[tokio::test]
async fn default_template_resolves_pure_env() {
    skip_without_nix!();
    let dir = tempfile::TempDir::new().expect("temp dir");
    assert!(
        nix_env::ensure_default_flake(dir.path()).expect("ensure"),
        "nix available => default flake should be created"
    );
    let (flake, _) = nix_env::flake_paths(dir.path());
    assert_eq!(
        std::fs::read_to_string(&flake).expect("read"),
        nix_env::DEFAULT_FLAKE
    );

    let cache = NixEnvCache::new();
    let env = cache
        .resolve_fully(dir.path())
        .await
        .expect("resolve")
        .expect("pure env");
    let path = env.get("PATH").expect("PATH");
    assert!(path.contains("/nix/store/"), "PATH={path}");
    let bash = nix_env::resolve_program("bash", &env).expect("bash in pure env");
    assert!(bash.starts_with("/nix/store/"), "bash={bash}");
    assert!(env.contains_key("HOME"), "HOME filled from host");
}

/// 端到端：bash 工具在纯净环境中执行，`which jq` 落在 nix store；
/// 且宿主污染变量不泄漏进纯净环境。
#[tokio::test]
#[allow(clippy::literal_string_with_formatting_args)] // `${VAR:-default}` 是 shell 语法
async fn bash_runs_inside_pure_env() {
    skip_without_nix!();
    let dir = tempfile::TempDir::new().expect("temp dir");
    nix_env::ensure_default_flake(dir.path()).expect("ensure");
    // 冷构建可能耗时数分钟：先完整解析，再让 bash 调用命中缓存
    let cache = NixEnvCache::new();
    cache
        .resolve_fully(dir.path())
        .await
        .expect("resolve")
        .expect("pure env");
    let tool = BashTool::new()
        .with_base_dir(Some(dir.path().to_path_buf()))
        .with_nix_env(cache);
    let result = tool
        .execute(
            nomic_tools::BashParams {
                command: "command -v jq && echo \"MARKER=${NOMIC_TEST_POLLUTION:-unset}\"".into(),
                timeout: Some(300.0),
            },
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect("bash in pure env");
    let text = text_of(&result);
    let jq_path = text.lines().next().unwrap_or_default();
    assert!(jq_path.starts_with("/nix/store/"), "jq at: {jq_path}");
    assert!(text.contains("MARKER=unset"), "{text}");
    assert!(
        result.details.is_none(),
        "no fallback note: {:?}",
        result.details
    );
}

/// 修改 flake 后 mtime 变化触发重解析（第二次解析走 nix eval 缓存，应
/// 很快完成）。
#[tokio::test]
async fn flake_change_triggers_re_resolve() {
    skip_without_nix!();
    let dir = tempfile::TempDir::new().expect("temp dir");
    nix_env::ensure_default_flake(dir.path()).expect("ensure");
    let cache = NixEnvCache::new();
    let first = cache
        .resolve_fully(dir.path())
        .await
        .expect("first resolve")
        .expect("env");

    // 追加一个环境变量到 shellHook，改 mtime 也改内容
    let (flake, _) = nix_env::flake_paths(dir.path());
    let mut content = std::fs::read_to_string(&flake).expect("read");
    content = content.replace(
        "packages = with pkgs;",
        "shellHook = ''\n              export NOMIC_RE_RESOLVED=1\n            '';\n            packages = with pkgs;",
    );
    std::fs::write(&flake, content).expect("rewrite");

    let second = cache
        .resolve_fully(dir.path())
        .await
        .expect("re-resolve")
        .expect("env");
    assert!(
        !std::sync::Arc::ptr_eq(&first, &second),
        "cache should re-resolve after flake change"
    );
    assert_eq!(
        second.get("NOMIC_RE_RESOLVED").map(String::as_str),
        Some("1")
    );
}

/// 无 flake 的 project：`env_for` 返回未启用，bash 工具静默走宿主
/// 环境（无尾注）。
#[tokio::test]
async fn missing_flake_means_not_enabled() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let cache = NixEnvCache::new();
    let result = cache.env_for(dir.path()).await.expect("resolve");
    assert!(result.is_none(), "no flake => not enabled");

    let tool = BashTool::new()
        .with_base_dir(Some(dir.path().to_path_buf()))
        .with_nix_env(cache);
    let result = tool
        .execute(
            nomic_tools::BashParams {
                command: "echo host".into(),
                timeout: Some(30.0),
            },
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect("host run");
    assert_eq!(text_of(&result).trim_end(), "host");
    assert!(result.details.is_none());
}

/// ensure_default_flake 的幂等与无 nix 时的 no-op 语义（门控部分见上）。
#[test]
fn ensure_default_flake_idempotent() {
    skip_without_nix!();
    let dir = tempfile::TempDir::new().expect("temp dir");
    assert!(nix_env::ensure_default_flake(dir.path()).expect("first"));
    let (flake, _) = nix_env::flake_paths(dir.path());
    std::fs::write(&flake, "# user edited\n").expect("edit");
    assert!(
        !nix_env::ensure_default_flake(dir.path()).expect("second"),
        "existing flake must not be overwritten"
    );
    assert_eq!(
        std::fs::read_to_string(&flake).expect("read"),
        "# user edited\n"
    );
}
