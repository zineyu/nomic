//! 四工具的真实行为集成测试（临时目录 + 真实进程）。

use nomic_core::{AgentTool, ToolUpdateCallback};
use nomic_tools::{BashTool, EditTool, ReadTool, WriteTool};
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
    let result = WriteTool
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

    let result = ReadTool
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

    let result = ReadTool
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
async fn edit_applies_and_returns_diff() {
    let dir = temp_dir();
    let path = dir.join("code.rs");
    std::fs::write(
        &path,
        "fn main() {\n    println!(\"a\");\n    println!(\"b\");\n}\n",
    )
    .expect("write fixture");

    let result = EditTool
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

    EditTool
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

    let err = EditTool
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
    let result = BashTool
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

    let err = BashTool
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
    let err = BashTool
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
    let err_or_ok = BashTool
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
