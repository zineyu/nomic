//! 四工具的真实行为集成测试（临时目录 + 真实进程）。

use nomic_core::{AgentTool, ToolError, ToolUpdateCallback};
use nomic_skills::{ProjectDiscovery, SkillResolver, SkillRoot, SkillScope};
use nomic_tools::{
    AskUserAnswer, AskUserQuestion, AskUserQuestionTool, BashTool, CUSTOM_OPTION, EditTool,
    QuestionSink, ReadTool, WriteTool,
};
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

#[tokio::test]
async fn write_creates_parent_dirs() {
    let dir = temp_dir();
    let path = dir.join("a/b/c.txt");
    let result = WriteTool::new()
        .execute(
            serde_json::from_value(
                serde_json::json!({"path": path.display().to_string(), "content": "hello"}),
            )
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("write");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.contains("Successfully wrote 5 bytes"));
    assert_eq!(std::fs::read_to_string(&path).expect("read back"), "hello");
}

#[tokio::test]
async fn read_truncates_and_guides_pagination() {
    let dir = temp_dir();
    let path = dir.join("big.txt");
    let content = (1..=3000)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, content).expect("write fixture");

    let result = ReadTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({"path": path.display().to_string()}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("read");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.contains("line 1"));
    assert!(!text.text.contains("line 3000"));
    assert!(
        text.text
            .contains("[Showing lines 1-2000 of 3000. Use offset=2001 to continue.]"),
        "missing pagination hint: {}",
        &text.text[text.text.len().saturating_sub(200)..]
    );
}

#[tokio::test]
async fn read_offset_limit() {
    let dir = temp_dir();
    let path = dir.join("small.txt");
    std::fs::write(&path, "a\nb\nc\nd").expect("write fixture");

    let result = ReadTool::new()
        .execute(
            serde_json::from_value(
                serde_json::json!({"path": path.display().to_string(), "offset": 2, "limit": 2}),
            )
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("read");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(
        text.text,
        "b\nc\n\n[1 more lines in file. Use offset=4 to continue.]"
    );
}

#[tokio::test]
async fn read_skill_uri_resolves_and_paginates() {
    let dir = temp_dir();
    let skills_dir = dir.join("skills");
    let review_dir = skills_dir.join("rust-review");
    std::fs::create_dir_all(&review_dir).expect("skill dir");
    std::fs::write(
        review_dir.join("SKILL.md"),
        "---\ndescription: Review Rust code\n---\nline 1\nline 2\nline 3\n",
    )
    .expect("write skill");
    let resolver = SkillResolver::new(
        &dir,
        ProjectDiscovery::Roots(Vec::new()),
        vec![SkillRoot {
            path: skills_dir,
            scope: SkillScope::Project,
        }],
    )
    .expect("resolver");

    let result = ReadTool::with_skill_resolver(resolver)
        .execute(
            serde_json::from_value(
                serde_json::json!({"path": "skill://rust-review", "offset": 2, "limit": 1}),
            )
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("read skill");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(
        text.text,
        "line 2\n\n[1 more lines in file. Use offset=3 to continue.]"
    );
    let details = result.details.expect("details");
    assert_eq!(details["source"]["kind"].as_str(), Some("skill"));
    assert_eq!(details["source"]["name"].as_str(), Some("rust-review"));
    assert!(
        details["source"]["path"]
            .as_str()
            .expect("path")
            .ends_with("SKILL.md")
    );
}

#[tokio::test]
async fn read_skill_uri_without_resolver_is_actionable_error() {
    let error = ReadTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({"path": "skill://rust-review"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Skill reading is not configured")
    );
}

#[tokio::test]
async fn read_missing_skill_lists_available_names() {
    let dir = temp_dir();
    let skills_dir = dir.join("skills");
    let existing_dir = skills_dir.join("existing");
    std::fs::create_dir_all(&existing_dir).expect("skill dir");
    std::fs::write(existing_dir.join("SKILL.md"), "# Existing\n").expect("write skill");
    let resolver = SkillResolver::new(
        &dir,
        ProjectDiscovery::Roots(Vec::new()),
        vec![SkillRoot {
            path: skills_dir,
            scope: SkillScope::Project,
        }],
    )
    .expect("resolver");

    let error = ReadTool::with_skill_resolver(resolver)
        .execute(
            serde_json::from_value(serde_json::json!({"path": "skill://missing"})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("skill://missing"));
    assert!(error.to_string().contains("available: existing"));
}

#[tokio::test]
async fn read_skill_sub_resource_file_dir_and_traversal() {
    let dir = temp_dir();
    let skills_dir = dir.join("skills");
    let demo_dir = skills_dir.join("demo");
    std::fs::create_dir_all(demo_dir.join("scripts")).expect("skill dir");
    std::fs::create_dir_all(demo_dir.join("references")).expect("skill dir");
    std::fs::write(demo_dir.join("SKILL.md"), "demo body").expect("write skill");
    std::fs::write(demo_dir.join("scripts/run.sh"), "echo one\necho two\n").expect("write");
    std::fs::write(demo_dir.join("notes.md"), "notes").expect("write");
    std::fs::write(skills_dir.join("secret.txt"), "secret").expect("write");
    let resolver = SkillResolver::new(
        &dir,
        ProjectDiscovery::Roots(Vec::new()),
        vec![SkillRoot {
            path: skills_dir.clone(),
            scope: SkillScope::Project,
        }],
    )
    .expect("resolver");
    let tool = ReadTool::with_skill_resolver(resolver);
    let read = |path: &str| {
        let tool = tool.clone();
        let path = path.to_string();
        async move {
            tool.execute(
                serde_json::from_value(serde_json::json!({"path": path})).expect("params"),
                CancellationToken::new(),
                no_update(),
            )
            .await
        }
    };
    let text_of = |result: nomic_core::ToolResult| {
        let nomic_ai::UserContent::Text(text) = &result.content[0] else {
            panic!("expected text")
        };
        text.text.clone()
    };

    // 文件子资源：内容与现有截断/分页契约一致，details 标注 resource
    let result = read("skill://demo/scripts/run.sh")
        .await
        .expect("read file");
    assert_eq!(text_of(result.clone()), "echo one\necho two\n");
    let details = result.details.expect("details");
    assert_eq!(details["source"]["kind"].as_str(), Some("skill"));
    assert_eq!(
        details["source"]["resource"].as_str(),
        Some("scripts/run.sh")
    );

    // 尾随斜杠的空子路径：退化为正文（与无子路径一致）
    let result = read("skill://demo/").await.expect("read trailing slash");
    assert_eq!(text_of(result), "demo body");

    // 目录子资源：返回清单，目录以 / 结尾，按名称排序
    let result = read("skill://demo").await.expect("read root");
    assert_eq!(text_of(result), "demo body"); // 无子路径仍是正文
    let result = read("skill://demo/.").await.expect("read dot");
    assert_eq!(text_of(result), "demo body"); // `.` 同样退化为正文
    let result = read("skill://demo/scripts").await.expect("read dir");
    assert_eq!(text_of(result.clone()), "run.sh");
    assert_eq!(
        result.details.expect("details")["source"]["resource"].as_str(),
        Some("scripts/")
    );

    // skill 根目录本身作为子资源（显式 `/` 以外想看全部文件时可读根清单）
    // 穿越与绝对路径：可读错误
    let error = read("skill://demo/../secret.txt").await.unwrap_err();
    assert!(error.to_string().contains("skill://demo/../secret.txt"));
    let error = read("skill://demo/scripts/../../secret.txt")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("inside the skill directory"));

    // 根内不存在的资源
    let error = read("skill://demo/scripts/missing.sh").await.unwrap_err();
    assert!(error.to_string().contains("missing.sh"));
}

#[tokio::test]
async fn edit_applies_and_returns_diff() {
    let dir = temp_dir();
    let path = dir.join("code.rs");
    std::fs::write(
        &path,
        "fn main() {\n    println!(\"a\");\n    println!(\"b\");\n}\n",
    )
    .expect("write fixture");

    let result = EditTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({
                "path": path.display().to_string(),
                "edits": [{"oldText": "println!(\"a\");", "newText": "println!(\"z\");"}],
            }))
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("edit");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "fn main() {\n    println!(\"z\");\n    println!(\"b\");\n}\n"
    );
    let details = result.details.expect("details");
    assert!(
        details["diff"]
            .as_str()
            .expect("diff")
            .contains("-    println!(\"a\");")
    );
    assert_eq!(details["first_changed_line"], 2);
}

#[tokio::test]
async fn edit_preserves_crlf() {
    let dir = temp_dir();
    let path = dir.join("win.txt");
    std::fs::write(&path, "one\r\ntwo\r\nthree\r\n").expect("write fixture");

    EditTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({
                "path": path.display().to_string(),
                "edits": [{"oldText": "two", "newText": "TWO"}],
            }))
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("edit");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "one\r\nTWO\r\nthree\r\n"
    );
}

#[tokio::test]
async fn edit_non_unique_match_is_error_for_model() {
    let dir = temp_dir();
    let path = dir.join("dup.txt");
    std::fs::write(&path, "x\nx\n").expect("write fixture");

    let err = EditTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({
                "path": path.display().to_string(),
                "edits": [{"oldText": "x", "newText": "y"}],
            }))
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("2 locations"), "{err}");
}

#[tokio::test]
async fn bash_captures_output_and_exit_code() {
    let result = BashTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({"command": "echo hello"})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("bash");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(text.text, "hello\n");

    let err = BashTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({"command": "echo oops && exit 3"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("oops"), "{err}");
    assert!(
        err.to_string().contains("Command exited with code 3"),
        "{err}"
    );
}

#[tokio::test]
async fn bash_timeout_kills_process() {
    let start = std::time::Instant::now();
    let err = BashTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({"command": "sleep 30", "timeout": 1}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("timed out after 1 seconds"),
        "{err}"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
}

#[tokio::test]
async fn bash_truncates_long_output_to_temp_file() {
    let err_or_ok = BashTool::new()
        .execute(
            serde_json::from_value(serde_json::json!({"command": "seq 1 5000"})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("bash");
    let nomic_ai::UserContent::Text(text) = &err_or_ok.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.contains("5000"));
    assert!(
        text.text
            .contains("[Showing lines 3001-5000 of 5000. Full output:"),
        "missing truncation hint: {}",
        &text.text[text.text.len().saturating_sub(200)..]
    );
    // 临时文件存在且包含完整输出
    let details = err_or_ok.details.expect("details");
    let path = details["full_output_path"].as_str().expect("path");
    let full = std::fs::read_to_string(path).expect("read full output");
    assert!(full.contains("1\n2\n3"));
}

// ── grep ─────────────────────────────────────────────────────────────────────

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
    // 二进制文件：searcher 的 NUL 检测应直接跳过
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
async fn grep_matches_sorted_and_gitignore_aware() {
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
        ],
        "gitignore 排除 target/、默认跳过隐藏目录，结果按路径+行号排序"
    );
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
    // include_hidden 放开隐藏目录
    let out = run_grep(serde_json::json!({
        "pattern": "todo in hidden",
        "path": dir.display().to_string(),
        "include_hidden": true,
    }))
    .await;
    assert!(out.contains(".hidden/secret.rs:1:"), "{out}");
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
        ],
        "任意深度文件名匹配，gitignore 排除 target/，跳过隐藏目录"
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
    let out = run_find(serde_json::json!({
        "pattern": "*.rs",
        "path": dir.display().to_string(),
        "include_hidden": true,
    }))
    .await;
    assert!(out.contains(".hidden/secret.rs"), "{out}");
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

// ── ask_user_question ──────────────────────────────────────────────────────

/// 记录收到的问题（断言工具侧契约）并按预设回答回传。
struct RecordingSink {
    received: std::sync::Mutex<Vec<AskUserQuestion>>,
    preset: std::sync::Mutex<Option<AskUserAnswer>>,
}

#[async_trait::async_trait]
impl QuestionSink for RecordingSink {
    async fn ask(
        &self,
        question: AskUserQuestion,
        _cancel: CancellationToken,
    ) -> Result<AskUserAnswer, ToolError> {
        self.received.lock().expect("lock").push(question);
        self.preset
            .lock()
            .expect("lock")
            .take()
            .ok_or_else(|| ToolError::new("no preset answer"))
    }
}

/// 经类型擦除的 [`nomic_core::DynTool`] 全链路执行：JSON 参数反序列化 →
/// 问题宿收到（含自动追加的自定义选项）→ 回答回喂模型。
#[tokio::test]
async fn ask_user_question_flows_through_erased_tool() {
    let sink = std::sync::Arc::new(RecordingSink {
        received: std::sync::Mutex::new(Vec::new()),
        preset: std::sync::Mutex::new(Some(AskUserAnswer {
            answers: vec!["Rust".to_string()],
            custom: None,
        })),
    });
    let tool = nomic_core::DynTool::new(AskUserQuestionTool::new(sink.clone()));
    assert_eq!(tool.name(), "ask_user_question");
    // 发送给 provider 的工具定义：JSON Schema 含三种类型
    let schema = tool.definition().parameters.to_string();
    for kind in ["single_choice", "multiple_choice", "fill_in"] {
        assert!(schema.contains(kind), "schema 缺 {kind}: {schema}");
    }

    let result = tool
        .execute(
            serde_json::json!({
                "question": "语言？",
                "kind": "single_choice",
                "options": ["Rust", "Go"],
            }),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("answer");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(
        text.text.contains("User answered (single_choice): Rust"),
        "{}",
        text.text
    );
    // 问题宿收到完整问题：末尾自动追加自定义选项
    let received = sink.received.lock().expect("lock");
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0].options.last().map(String::as_str),
        Some(CUSTOM_OPTION)
    );
    assert_eq!(received[0].options.len(), 3);
    drop(received);
}

/// 参数校验：单选/多选缺 options 时报错（错误文本回喂模型）。
#[tokio::test]
async fn ask_user_question_choice_requires_options() {
    let tool = nomic_core::DynTool::new(AskUserQuestionTool::new(std::sync::Arc::new(
        RecordingSink {
            received: std::sync::Mutex::new(Vec::new()),
            preset: std::sync::Mutex::new(None),
        },
    )));
    let error = tool
        .execute(
            serde_json::json!({"question": "语言？", "kind": "single_choice"}),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("options are required"),
        "{error}"
    );
}

/// 填空问题：options 被忽略，问题宿收到空选项列表。
#[tokio::test]
async fn ask_user_question_fill_in_ignores_options() {
    let sink = std::sync::Arc::new(RecordingSink {
        received: std::sync::Mutex::new(Vec::new()),
        preset: std::sync::Mutex::new(Some(AskUserAnswer {
            answers: vec!["a@b.c".to_string()],
            custom: Some("a@b.c".to_string()),
        })),
    });
    let tool = nomic_core::DynTool::new(AskUserQuestionTool::new(sink.clone()));
    let result = tool
        .execute(
            serde_json::json!({
                "question": "邮箱？",
                "kind": "fill_in",
                "options": ["a@b.c"],
            }),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("answer");
    let received = sink.received.lock().expect("lock");
    assert!(received[0].options.is_empty(), "填空忽略 options");
    drop(received);
    let details = result.details.expect("details");
    assert_eq!(details["custom"], "a@b.c");
}
