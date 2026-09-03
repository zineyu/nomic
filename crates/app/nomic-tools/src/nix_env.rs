//! nix 纯净环境：定义模板、env 解析与缓存（ADR-0041）。
//!
//! project 的环境定义为 `<project>/.nomic/flake.nix`（devShell）。
//! `nix develop` 求值有数百 ms～秒级延迟，不能摊到每次 bash 调用：
//! [`NixEnvCache`] 首次/定义变更后解析一次（`nix develop
//! --ignore-environment --command env -0`），之后 plain bash + 注入缓存
//! env。nix 不可用 / 定义缺失 / 求值失败一律回退宿主环境（可用性优先）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// 单次 bash 调用等待进行中的环境解析的上限：超过即本次回退宿主环境，
/// 不阻塞工具调用（首个调用恰好赶上冷构建时尤其重要）。
const RESOLVE_WAIT: Duration = Duration::from_secs(5);

/// 环境解析自身的总预算：首次构建需下载 nixpkgs 与包，宽限放大。
const RESOLVE_BUDGET: Duration = Duration::from_mins(10);

/// 解析输出的哨兵变量：`nix develop --command env -0 <MARKER>=1`，
/// 解析后必须存在（缺失 = 输出被 shellHook 等污染 / 非预期输出）。
const ENV_MARKER: &str = "__NOMIC_ENV_BEGIN__";

/// project 环境定义文件（相对 project 根）。
pub const FLAKE_RELATIVE_PATH: &str = ".nomic/flake.nix";

/// 默认模板：bash + jq + curl + git + gh（coreutils 等基础工具由 stdenv
/// 隐式提供）。
pub const DEFAULT_FLAKE: &str = r#"{
  description = "nomic project shell (edit via nix://shell)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      forEachSystem = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    in
    {
      devShells = forEachSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              bashInteractive
              jq
              curl
              git
              gh
            ];
          };
        }
      );
    };
}
"#;

/// project 的 flake 定义路径与 lock 路径。
#[must_use]
pub fn flake_paths(project: &Path) -> (PathBuf, PathBuf) {
    let flake = project.join(FLAKE_RELATIVE_PATH);
    let lock = project.join(".nomic").join("flake.lock");
    (flake, lock)
}

/// 解析 `env -0` 输出（NUL 分隔的 `KEY=VALUE`）为环境表。
/// 无 `=` 的条目跳过；value 中允许含 `=`。
#[must_use]
pub fn parse_env_zero(bytes: &[u8]) -> HashMap<String, String> {
    let mut env = HashMap::new();
    for entry in bytes.split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Ok(entry) = std::str::from_utf8(entry) else {
            continue; // 非 UTF-8 变量值对 bash 调用无意义，跳过
        };
        let Some((key, value)) = entry.split_once('=') else {
            continue;
        };
        env.insert(key.to_string(), value.to_string());
    }
    env
}

/// pure 模式会清掉 `HOME`/`USER`/`TMPDIR`，但工具运行需要可写的 HOME
/// 等：缺失时从宿主进程环境补齐（不覆盖 nix 环境已有的值）。
pub fn fill_missing_host_vars<S: std::hash::BuildHasher>(env: &mut HashMap<String, String, S>) {
    for key in ["HOME", "USER", "TMPDIR"] {
        if !env.contains_key(key)
            && let Ok(value) = std::env::var(key)
        {
            env.insert(key.to_string(), value);
        }
    }
}

/// 缓存键：flake.nix 与 flake.lock 的 mtime（缺失为 `None`）。
/// 文件未变即命中缓存；agent 经 `nix://shell` 改写后 mtime 变化自然失效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlakeKey {
    /// flake.nix 的 mtime
    pub flake_mtime: Option<SystemTime>,
    /// flake.lock 的 mtime
    pub lock_mtime: Option<SystemTime>,
}

impl FlakeKey {
    /// stat 当前文件状态生成键（每次 bash 调用一次的廉价检查）。
    #[must_use]
    pub fn stat(project: &Path) -> Self {
        let (flake, lock) = flake_paths(project);
        let mtime = |path: &Path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        };
        Self {
            flake_mtime: mtime(&flake),
            lock_mtime: mtime(&lock),
        }
    }

    /// flake.nix 是否存在（缺失 = 未启用 nix 环境，非错误）。
    #[must_use]
    pub const fn has_flake(&self) -> bool {
        self.flake_mtime.is_some()
    }
}

/// 解析结果：`Ok(None)` = 未启用（无 flake）；`Ok(Some(env))` = 纯净环境；
/// `Err(reason)` = 回退宿主环境的原因（面向模型/用户的提示文本）。
type ResolveResult = Result<Option<Arc<HashMap<String, String>>>, Arc<str>>;

/// 缓存状态：键含 project 路径（交互端可切换 session project）。
#[derive(Debug)]
struct CacheState {
    key: Option<(PathBuf, FlakeKey)>,
    result: Option<ResolveResult>,
    resolving: bool,
}

/// 会话级 nix 环境缓存（ADR-0041）：首次/定义变更后解析一次 devShell
/// 环境，之后 bash 调用直接注入缓存 env。解析在后台任务中进行，
/// 单次调用最多等待 `RESOLVE_WAIT`，超时回退宿主环境不阻塞工具。
pub struct NixEnvCache {
    state: tokio::sync::Mutex<CacheState>,
    notify: tokio::sync::Notify,
}

impl std::fmt::Debug for NixEnvCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NixEnvCache").finish_non_exhaustive()
    }
}

impl Default for NixEnvCache {
    fn default() -> Self {
        Self {
            state: tokio::sync::Mutex::new(CacheState {
                key: None,
                result: None,
                resolving: false,
            }),
            notify: tokio::sync::Notify::new(),
        }
    }
}

impl NixEnvCache {
    /// 空缓存。
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 后台预解析（session/project 初始化时调用，给首个 bash 调用提前量）；
    /// 已就绪/进行中/无 flake 时为 no-op。
    pub fn prewarm(self: &Arc<Self>, project: &Path) {
        let key = FlakeKey::stat(project);
        if !key.has_flake() {
            return;
        }
        let Ok(mut state) = self.state.try_lock() else {
            return; // 另一调用正在裁决，交由它处理
        };
        if state.key.as_ref() == Some(&(project.to_path_buf(), key))
            && (state.result.is_some() || state.resolving)
        {
            return;
        }
        state.key = Some((project.to_path_buf(), key));
        state.result = None;
        state.resolving = true;
        drop(state);
        self.spawn_resolve(project.to_path_buf(), key);
    }

    /// 取 project 的执行环境（详见类型别名 `ResolveResult` 的语义）。
    /// 解析在进行中时最多等待 `RESOLVE_WAIT`，超时本次回退宿主环境。
    pub async fn env_for(self: &Arc<Self>, project: &Path) -> ResolveResult {
        let key = FlakeKey::stat(project);
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable(); // 先注册等待者再查状态，消除 notify 竞态
        if let Some(result) = self.ensure_started(project, key).await {
            return result;
        }
        let _ = tokio::time::timeout(RESOLVE_WAIT, notified).await;
        let result = {
            let state = self.state.lock().await;
            if state.key.as_ref() == Some(&(project.to_path_buf(), key)) {
                state.result.clone()
            } else {
                None
            }
        };
        result.unwrap_or_else(|| {
            Err(Arc::from(
                "nix environment is still resolving (first build can take minutes)",
            ))
        })
    }

    /// 等待解析完成（上限 `RESOLVE_BUDGET` + 余量）：冷构建可能耗时
    /// 数分钟，供测试与显式预解析使用；常规 bash 调用走 [`Self::env_for`]。
    pub async fn resolve_fully(self: &Arc<Self>, project: &Path) -> ResolveResult {
        let key = FlakeKey::stat(project);
        let deadline = tokio::time::Instant::now() + RESOLVE_BUDGET + Duration::from_secs(30);
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(result) = self.ensure_started(project, key).await {
                return result;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(Arc::from(
                    "nix environment resolution did not finish in time",
                ));
            }
        }
    }

    /// 确保 (project, key) 的解析已启动；已有结果则直接返回。
    async fn ensure_started(
        self: &Arc<Self>,
        project: &Path,
        key: FlakeKey,
    ) -> Option<ResolveResult> {
        {
            let mut state = self.state.lock().await;
            let target = (project.to_path_buf(), key);
            if state.key.as_ref() == Some(&target) {
                return state.result.clone();
            }
            state.key = Some(target);
            state.result = None;
            state.resolving = true;
        }
        self.spawn_resolve(project.to_path_buf(), key);
        None
    }

    /// 后台解析任务：完成后仅当键仍一致（期间文件/ project 未再变）
    /// 才写入结果并唤醒等待者。
    fn spawn_resolve(self: &Arc<Self>, project: PathBuf, key: FlakeKey) {
        let cache = Arc::clone(self);
        tokio::spawn(async move {
            let result =
                match tokio::time::timeout(RESOLVE_BUDGET, resolve_env(&project, key)).await {
                    Ok(result) => result,
                    Err(_) => Err(Arc::from(
                        "nix environment resolution timed out after 10 minutes",
                    )),
                };
            {
                let mut state = cache.state.lock().await;
                if state.key.as_ref() == Some(&(project, key)) {
                    state.result = Some(result);
                    state.resolving = false;
                }
            }
            cache.notify.notify_waiters();
        });
    }
}

/// 实际解析：`nix develop --ignore-environment` 进 devShell 后导出完整
/// 环境。哨兵变量校验输出完整性；HOME/USER/TMPDIR 缺失时从宿主补齐。
async fn resolve_env(project: &Path, key: FlakeKey) -> ResolveResult {
    if !key.has_flake() {
        return Ok(None);
    }
    let (flake, _) = flake_paths(project);
    let nomic_dir = flake.parent().expect("flake path has .nomic parent");
    let installable = format!("path:{}", nomic_dir.display());
    let marker_arg = format!("{ENV_MARKER}=1");
    let output = tokio::process::Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "develop",
            "--ignore-environment",
            &installable,
            "--command",
            "env",
            "-0",
            &marker_arg,
        ])
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| Arc::<str>::from(format!("nix is not available: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let hint = if stderr.contains("not tracked") {
            "\nHint: the flake is inside a Git repo; run `git add -N .nomic/flake.nix` so Nix can see it."
        } else {
            ""
        };
        return Err(Arc::<str>::from(format!(
            "nix develop failed: {}{hint}",
            tail.join("\n")
        )));
    }
    let mut env = parse_env_zero(&output.stdout);
    if env.remove(ENV_MARKER).is_none() {
        return Err(Arc::<str>::from(
            "nix develop output missing the environment marker (shellHook stdout noise?)",
        ));
    }
    fill_missing_host_vars(&mut env);
    Ok(Some(Arc::new(env)))
}

/// 在解析出的环境 PATH 中定位可执行文件，返回绝对路径；找不到返回
/// `None`（调用方回退为裸程序名，由宿主 PATH 解析）。`env_clear` 注入
/// 后子进程 PATH 与宿主不同，必须显式解析，不能依赖 execvp 的宿主 PATH。
#[must_use]
pub fn resolve_program<S: std::hash::BuildHasher>(
    name: &str,
    env: &HashMap<String, String, S>,
) -> Option<String> {
    let path = env.get("PATH")?;
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(name);
        if is_executable(&candidate) {
            return Some(candidate.display().to_string());
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// nix 可执行且在 PATH 中（`nix --version` 探测，一次调用即可）。
#[must_use]
fn nix_on_path() -> bool {
    std::process::Command::new("nix")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// 惰性创建默认环境定义（ADR-0041）：nix 可用且 flake.nix 缺失时写入
/// 默认模板。flake 在 git 仓库内只对 git 可见的文件生效，因此写入后
/// best-effort `git add -N`（intent-to-add 不暂存内容，只让 nix 可见）。
/// 返回是否创建了文件。
pub fn ensure_default_flake(project: &Path) -> std::io::Result<bool> {
    if !nix_on_path() {
        return Ok(false);
    }
    let (flake, _) = flake_paths(project);
    if flake.exists() {
        return Ok(false);
    }
    let parent = flake.parent().expect("flake path has .nomic parent");
    std::fs::create_dir_all(parent)?;
    std::fs::write(&flake, DEFAULT_FLAKE)?;
    tracing::info!(flake = %flake.display(), "created default nix project environment");
    let status = std::process::Command::new("git")
        .args([
            "-C",
            &project.display().to_string(),
            "add",
            "-N",
            "--",
            ".nomic/flake.nix",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if let Ok(status) = status
        && !status.success()
    {
        tracing::debug!("git add -N skipped (not a git repo or git unavailable)");
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_env_zero_basic() {
        let bytes = b"PATH=/nix/store/bin\0HOME=/home/u\0EMPTY=\0";
        let env = parse_env_zero(bytes);
        assert_eq!(env.get("PATH").expect("PATH"), "/nix/store/bin");
        assert_eq!(env.get("HOME").expect("HOME"), "/home/u");
        assert_eq!(env.get("EMPTY").expect("EMPTY"), "");
        assert_eq!(env.len(), 3);
    }

    #[test]
    fn parse_env_zero_skips_malformed_and_keeps_equals_in_value() {
        let bytes = b"NO_EQUALS\0FLAGS=-a=b\0\xFF\xFE=x\0GOOD=1\0";
        let env = parse_env_zero(bytes);
        assert!(!env.contains_key("NO_EQUALS"));
        assert_eq!(env.get("FLAGS").expect("FLAGS"), "-a=b");
        assert_eq!(env.get("GOOD").expect("GOOD"), "1");
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn fill_missing_host_vars_only_fills_absent() {
        let host_home = std::env::var("HOME").expect("test env has HOME");
        let mut env = HashMap::from([("USER".to_string(), "nix-user".to_string())]);
        fill_missing_host_vars(&mut env);
        assert_eq!(env.get("HOME").expect("filled"), &host_home);
        assert_eq!(env.get("USER").expect("untouched"), "nix-user");
    }

    #[test]
    fn flake_paths_are_under_nomic_dir() {
        let (flake, lock) = flake_paths(Path::new("/ws"));
        assert_eq!(flake, Path::new("/ws/.nomic/flake.nix"));
        assert_eq!(lock, Path::new("/ws/.nomic/flake.lock"));
    }

    #[test]
    fn flake_key_reflects_existence_and_change() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let missing = FlakeKey::stat(dir.path());
        assert!(!missing.has_flake());

        let (flake, _lock) = flake_paths(dir.path());
        std::fs::create_dir_all(flake.parent().expect("parent")).expect("mkdir");
        std::fs::write(&flake, "{}").expect("write");
        let first = FlakeKey::stat(dir.path());
        assert!(first.has_flake());
        assert_eq!(first, FlakeKey::stat(dir.path())); // 未修改 = 命中

        // mtime 精度可能粗到秒级：直接 set 一个确定不同的 mtime
        let file = std::fs::File::options()
            .write(true)
            .open(&flake)
            .expect("open");
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(42))
            .expect("set mtime");
        let changed = FlakeKey::stat(dir.path());
        assert_ne!(first, changed);
    }

    #[test]
    fn default_template_contains_declared_packages() {
        for pkg in ["bashInteractive", "jq", "curl", "git", "gh"] {
            assert!(DEFAULT_FLAKE.contains(pkg), "template missing {pkg}");
        }
        assert!(DEFAULT_FLAKE.contains("devShells"));
        assert!(DEFAULT_FLAKE.contains("mkShell"));
    }
}
