//! grep/find 工具的真实行为集成测试（fff 索引 + 临时目录 fixture）。
//!
//! 语义矩阵（fff 引擎）：
//! - git 仓库（.git）：索引隐藏文件、遵守 .gitignore；
//! - jj 仓库（.jj）：隐藏目录跳过，但 ignore crate 把 .jj 视为 VCS 根，
//!   .gitignore 生效；
//! - 非 VCS 目录：隐藏目录跳过、.gitignore 不生效（fff 以硬编码的重型
//!   目录清单替代）；
//! - `.git`/`.jj` 内部路径始终在输出侧过滤。

use nomic_core::{AgentTool, ToolUpdateCallback};
use tokio_util::sync::CancellationToken;

fn no_update() -> ToolUpdateCallback {
    Box::new(|_| {})
}

fn temp_dir() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("nomic-tools-test-{nanos}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn grep_fixture() -> std::path::PathBuf {
    let dir = temp_dir();
    std::fs::create_dir_all(dir.join("src/nested")).expect("mkdir");
    std::fs::create_dir_all(dir.join("target")).expect("mkdir");
    std::fs::create_dir_all(dir.join(".hidden")).expect("mkdir");
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n// TODO: refactor\n").expect("write");
    std::fs::write(dir.join("src/nested/lib.rs"), "pub fn todo_list() {}\n").expect("write");
    std::fs::write(dir.join("README.md"), "# todo app\n").expect("write");
    std::fs::write(dir.join("target/build.rs"), "todo in ignored dir\n").expect("write");
    std::fs::write(dir.join(".hidden/secret.rs"), "todo in hidden dir\n").expect("write");
    std::fs::write(dir.join(".gitignore"), "target/\n").expect("write");
    // 二进制文件：NUL 检测应直接跳过
    std::fs::write(dir.join("blob.bin"), b"todo\x00binary").expect("write");
    dir
}

async fn run_grep(args: serde_json::Value) -> String {
    let result = nomic_tools::GrepTool::new()
        .execute(
            serde_json::from_value(args).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("grep");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    text.text.clone()
}

#[tokio::test]
async fn grep_matches_sorted_and_skips_hidden() {
    let dir = grep_fixture();
    let out = run_grep(serde_json::json!({
        "pattern": "todo",
        "path": dir.display().to_string(),
        "ignore_case": true,
    }))
    .await;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec![
            format!("{}/README.md:1: # todo app", dir.display()),
            format!("{}/src/main.rs:2: // TODO: refactor", dir.display()),
            format!(
                "{}/src/nested/lib.rs:1: pub fn todo_list() {{}}",
                dir.display()
            ),
            format!("{}/target/build.rs:1: todo in ignored dir", dir.display()),
        ],
        "非 git 目录：fff 跳过隐藏目录；.gitignore 不生效，target/ 会被搜到；\
         结果按路径+行号排序"
    );
}

#[tokio::test]
async fn grep_jj_repo_honors_gitignore_and_filters_vcs_internals() {
    // ignore crate 把 .jj 视为 VCS 根标记：jj 仓库内 .gitignore 生效；
    // fff 自身只认 .git（隐藏目录仍跳过）；.jj/ 内容在输出侧过滤
    let dir = grep_fixture();
    std::fs::create_dir_all(dir.join(".jj/repo")).expect("mkdir");
    std::fs::write(dir.join(".jj/repo/objects.rs"), "todo in vcs internals\n").expect("write");
    let out = run_grep(serde_json::json!({
        "pattern": "todo",
        "path": dir.display().to_string(),
        "ignore_case": true,
    }))
    .await;
    assert!(out.contains("README.md:1:"), "{out}");
    assert!(
        !out.contains("target/build.rs"),
        "jj 仓库内 gitignore 生效: {out}"
    );
    assert!(!out.contains(".jj/"), "VCS 内部路径被过滤: {out}");
}

#[tokio::test]
async fn grep_in_git_repo_honors_gitignore_and_includes_hidden() {
    let dir = grep_fixture();
    // fff 在 git 仓库内遵守 .gitignore 且索引隐藏文件（.jj 仍被输出侧过滤）
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    let out = run_grep(serde_json::json!({
        "pattern": "todo",
        "path": dir.display().to_string(),
        "ignore_case": true,
    }))
    .await;
    assert!(
        out.contains(".hidden/secret.rs:1:"),
        "git 仓库内搜索隐藏文件: {out}"
    );
    assert!(
        !out.contains("target/build.rs"),
        "gitignore 排除 target/: {out}"
    );
    assert!(!out.contains(".jj/"), "VCS 内部路径被过滤: {out}");
}

#[tokio::test]
async fn grep_literal_glob_and_hidden() {
    let dir = grep_fixture();
    // literal：正则元字符按字面处理
    let out = run_grep(serde_json::json!({
        "pattern": "fn main() {}",
        "path": dir.display().to_string(),
        "literal": true,
    }))
    .await;
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains("src/main.rs:1: fn main() {}"));
    // glob 过滤文件类型
    let out = run_grep(serde_json::json!({
        "pattern": "todo",
        "path": dir.display().to_string(),
        "glob": "*.md",
    }))
    .await;
    assert_eq!(out.lines().count(), 1, "{out}");
    // 非 git 目录：fff 不索引隐藏目录
    let out = run_grep(serde_json::json!({
        "pattern": "todo in hidden",
        "path": dir.display().to_string(),
    }))
    .await;
    assert!(out.starts_with("No matches found"), "{out}");
}

#[tokio::test]
async fn grep_single_file_root() {
    let dir = grep_fixture();
    let file = dir.join("src/main.rs");
    let out = run_grep(serde_json::json!({
        "pattern": "TODO",
        "path": file.display().to_string(),
    }))
    .await;
    assert_eq!(
        out,
        format!("{}:2: // TODO: refactor", file.display()),
        "单文件根直接搜索，不建索引"
    );
    // 单文件根的二进制契约：含 NUL 整体跳过
    let out = run_grep(serde_json::json!({
        "pattern": "todo",
        "path": dir.join("blob.bin").display().to_string(),
    }))
    .await;
    assert!(out.starts_with("No matches found"), "{out}");
}

#[tokio::test]
async fn grep_limit_reports_more_matches() {
    let dir = grep_fixture();
    let out = run_grep(serde_json::json!({
        "pattern": "todo",
        "path": dir.display().to_string(),
        "ignore_case": true,
        "limit": 2,
    }))
    .await;
    assert_eq!(out.lines().count(), 3, "{out}");
    assert!(
        out.contains("[Limit of 2 matches reached; more matches exist."),
        "{out}"
    );
}

#[tokio::test]
async fn grep_no_match_and_invalid_regex() {
    let dir = grep_fixture();
    let out = run_grep(serde_json::json!({
        "pattern": "nonexistent_pattern_xyz",
        "path": dir.display().to_string(),
    }))
    .await;
    assert!(out.starts_with("No matches found"), "{out}");

    let err = nomic_tools::GrepTool::new()
        .execute(
            serde_json::from_value(
                serde_json::json!({"pattern": "(", "path": dir.display().to_string()}),
            )
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Invalid regex"), "{err}");
}

// ── find ─────────────────────────────────────────────────────────────────────

async fn run_find(args: serde_json::Value) -> String {
    let result = nomic_tools::FindTool::new()
        .execute(
            serde_json::from_value(args).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("find");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    text.text.clone()
}

#[tokio::test]
async fn find_by_name_glob_and_kind() {
    let dir = grep_fixture();
    let out = run_find(serde_json::json!({
        "pattern": "*.rs",
        "path": dir.display().to_string(),
    }))
    .await;
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec![
            format!("{}/src/main.rs", dir.display()),
            format!("{}/src/nested/lib.rs", dir.display()),
            format!("{}/target/build.rs", dir.display()),
        ],
        "任意深度文件名匹配；非 git 目录：隐藏目录跳过、.jj 过滤、.gitignore 不生效"
    );
    // kind=dir 仅目录
    let out = run_find(serde_json::json!({
        "pattern": "src",
        "path": dir.display().to_string(),
        "kind": "dir",
    }))
    .await;
    assert_eq!(out, format!("{}/src", dir.display()));
}

#[tokio::test]
async fn find_path_glob_and_hidden() {
    let dir = grep_fixture();
    let out = run_find(serde_json::json!({
        "pattern": "src/**/*.rs",
        "path": dir.display().to_string(),
    }))
    .await;
    assert_eq!(out.lines().count(), 2, "{out}");
    // 非 git 目录：隐藏目录不索引，.jj 输出侧过滤
    let out = run_find(serde_json::json!({
        "pattern": "*.rs",
        "path": dir.display().to_string(),
    }))
    .await;
    assert!(!out.contains(".hidden/"), "{out}");
    assert!(!out.contains(".jj/"), "{out}");
}

#[tokio::test]
async fn find_limit_and_no_match() {
    let dir = grep_fixture();
    let out = run_find(serde_json::json!({
        "pattern": "*.rs",
        "path": dir.display().to_string(),
        "limit": 1,
    }))
    .await;
    assert!(
        out.contains("[Limit of 1 results reached; more results exist."),
        "{out}"
    );
    let out = run_find(serde_json::json!({
        "pattern": "*.xyz",
        "path": dir.display().to_string(),
    }))
    .await;
    assert!(out.starts_with("No files found"), "{out}");
}

#[tokio::test]
async fn smoke_real_jj_repo() {
    // 冒烟：在本仓库（jj+git colocate）上 grep/find 不泄漏 .jj/.git 内部文件
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root")
        .to_path_buf();
    let out = run_grep(serde_json::json!({
        "pattern": "struct GrepTool",
        "path": root,
        "glob": "*.rs",
    }))
    .await;
    eprintln!("SMOKE GREP:\n{out}");
    assert!(out.contains("grep.rs"), "{out}");
    assert!(!out.contains(".jj/") && !out.contains(".git/"), "{out}");
    let out = run_find(serde_json::json!({
        "pattern": "crates/app/*/src",
        "path": root,
    }))
    .await;
    eprintln!("SMOKE FIND:\n{out}");
    assert!(out.contains("crates/app/nomic-tools/src"), "{out}");
    assert!(!out.contains(".jj/") && !out.contains(".git/"), "{out}");
}
