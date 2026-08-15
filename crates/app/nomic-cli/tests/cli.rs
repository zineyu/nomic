//! nomic 二进制的进程级集成测试。
//!
//! 每个测试用独立的 `XDG_DATA_HOME`/`XDG_CONFIG_HOME`/`XDG_STATE_HOME` 指向
//! 临时目录，隔离用户真实配置、session 库与日志目录（滚动日志默认写入
//! 平台标准 state 目录下的 `nomic/logs`，不隔离会污染真实 state 目录）；
//! 隔离依赖 `dirs` 在 Linux 上遵循 XDG 环境变量（CI 仅在 Linux 运行测试）；
//! 不访问网络（provider 错误用例连接 127.0.0.1:1，立即拒绝）。

use std::path::Path;
use std::process::{Command, Output};

use nomic_ai::{Message, UserMessage, UserMessageContent};
use nomic_session::SessionStore;

/// 在隔离的 XDG 环境下运行 nomic 二进制；`dir` 为进程工作目录（缺省继承）。
fn run_in(args: &[&str], xdg_home: &Path, dir: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nomic"));
    command
        .args(args)
        .env("XDG_DATA_HOME", xdg_home)
        .env("XDG_CONFIG_HOME", xdg_home)
        .env("XDG_STATE_HOME", xdg_home);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    command.output().expect("spawn nomic")
}

/// 在隔离的 XDG 环境下运行 nomic 二进制。
fn run(args: &[&str], xdg_home: &Path) -> Output {
    run_in(args, xdg_home, None)
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn sessions_list_empty_database() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run(&["sessions", "list"], tmp.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("没有历史 session"));
}

#[tokio::test]
async fn sessions_list_shows_session_details() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("nomic").join("sessions.db");
    let store = SessionStore::open(&db).await.expect("open db");
    let id = store
        .create_session(Path::new("/tmp/project-alpha"))
        .await
        .expect("create session");
    store
        .append_message(
            &id,
            None,
            &Message::User(UserMessage {
                content: UserMessageContent::Text("hello".to_string()),
                timestamp: 1_785_000_000_000,
            }),
        )
        .await
        .expect("append message");
    drop(store);

    let output = run(&["sessions", "list"], tmp.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("hello"), "应展示会话标题：{out}");
    assert!(!out.contains(&id), "不应展示 session id：{out}");
    assert!(out.contains("/tmp/project-alpha"), "{out}");
    assert!(out.contains("1 条消息"), "{out}");
    assert!(out.contains("2026-"), "{out}");
}

/// provider 连接参数：指向无监听的 127.0.0.1:1，立即失败，无需真实网络。
/// session 选择发生在 provider 调用之前，因此即使最终非零退出，
/// stderr 中的 `session <ID>` 行仍能证明选择了哪个 session。
const DEAD_PROVIDER_ARGS: &[&str] = &[
    "--provider",
    "openai",
    "--model",
    "gpt-5.2",
    "--api-key",
    "test",
    "--base-url",
    "http://127.0.0.1:1/v1",
];

#[test]
fn provider_error_exits_nonzero() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut args = vec!["-p", "hi"];
    args.extend_from_slice(DEAD_PROVIDER_ARGS);
    let output = run(&args, tmp.path());
    assert!(
        !output.status.success(),
        "provider 错误应非零退出；stdout: {}",
        stdout(&output)
    );
}

/// 预建 A/B 两个项目目录与各自 session（A 全局更新更晚），返回路径与 id。
async fn seed_two_projects(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf, String, String) {
    let project_a = tmp.join("project-a");
    let project_b = tmp.join("project-b");
    std::fs::create_dir_all(&project_a).expect("mkdir a");
    std::fs::create_dir_all(&project_b).expect("mkdir b");
    let store = SessionStore::open(tmp.join("nomic").join("sessions.db"))
        .await
        .expect("open db");
    let session_b = store.create_session(&project_b).await.expect("create b");
    store
        .append_message(
            &session_b,
            None,
            &Message::User(UserMessage {
                content: UserMessageContent::Text("from b".to_string()),
                timestamp: 1000,
            }),
        )
        .await
        .expect("append b");
    // A 的消息更晚：A 是全局最近 session
    let session_a = store.create_session(&project_a).await.expect("create a");
    store
        .append_message(
            &session_a,
            None,
            &Message::User(UserMessage {
                content: UserMessageContent::Text("from a".to_string()),
                timestamp: 2000,
            }),
        )
        .await
        .expect("append a");
    (project_a, project_b, session_a, session_b)
}

#[test]
fn resume_empty_database_reports_no_sessions() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = run(&["resume"], tmp.path());
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("没有历史 session"));
}

#[tokio::test]
async fn resume_requires_tty_for_picker() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::open(tmp.path().join("nomic").join("sessions.db"))
        .await
        .expect("open db");
    store
        .create_session(Path::new("/tmp/project-alpha"))
        .await
        .expect("create session");
    drop(store);

    // 测试进程 stdout 是管道（非 TTY）：选择器不可用，必须报错并给出替代路径
    let output = run(&["resume"], tmp.path());
    assert!(!output.status.success(), "非 TTY 应失败");
    let err = stderr(&output);
    assert!(err.contains("--continue"), "应提示 --continue：{err}");
}

#[tokio::test]
async fn continue_resumes_session_of_current_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_project_a, project_b, session_a, session_b) = seed_two_projects(tmp.path()).await;

    let mut args = vec!["-p", "hi", "--continue"];
    args.extend_from_slice(DEAD_PROVIDER_ARGS);
    let output = run_in(&args, tmp.path(), Some(&project_b));
    assert!(!output.status.success(), "provider 必失败");
    let err = stderr(&output);
    // 恢复提示展示会话标题而非内部 id：标题证明选中的是当前目录的 B
    assert!(err.contains("from b"), "应选当前目录的 session B：{err}");
    assert!(!err.contains("from a"), "不应选全局最新的 A：{err}");
    assert!(!err.contains(&session_b), "不应展示 session id：{err}");
    assert!(!err.contains(&session_a), "不应展示 session id：{err}");
}

#[tokio::test]
async fn explicit_session_crosses_directory_with_warning() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_project_a, project_b, session_a, _session_b) = seed_two_projects(tmp.path()).await;

    let mut args = vec!["-p", "hi", "--session", &session_a];
    args.extend_from_slice(DEAD_PROVIDER_ARGS);
    let output = run_in(&args, tmp.path(), Some(&project_b));
    assert!(!output.status.success(), "provider 必失败");
    let err = stderr(&output);
    // 标题「from a」证明显式 --session 选中的是 A；id 不展示
    assert!(err.contains("from a"), "显式 --session 应选 A：{err}");
    assert!(!err.contains(&session_a), "不应展示 session id：{err}");
    assert!(err.contains("与当前目录不同"), "跨目录恢复应有提示：{err}");
}

#[tokio::test]
async fn continue_fails_in_directory_without_session() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_project_a, _project_b, _session_a, _session_b) = seed_two_projects(tmp.path()).await;
    let project_c = tmp.path().join("project-c");
    std::fs::create_dir_all(&project_c).expect("mkdir c");

    let mut args = vec!["-p", "hi", "--continue"];
    args.extend_from_slice(DEAD_PROVIDER_ARGS);
    let output = run_in(&args, tmp.path(), Some(&project_c));
    assert!(!output.status.success());
    let err = stderr(&output);
    assert!(
        err.contains("没有可恢复的 session"),
        "无本目录 session 应明确报错：{err}"
    );
}
