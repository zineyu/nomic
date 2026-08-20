//! base_dir 集成测试：workspace 严格归属下的相对路径解析与共享基准句柄。

use nomic_core::{AgentTool, ToolUpdateCallback};
use nomic_tools::{BashTool, ReadTool, WriteTool};
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

/// 最小 question sink（本文件的用例不触发提问）。
struct NoopSink;

#[async_trait::async_trait]
impl nomic_tools::QuestionSink for NoopSink {
    async fn ask(
        &self,
        _question: nomic_tools::AskUserQuestion,
        _cancel: CancellationToken,
    ) -> Result<nomic_tools::AskUserAnswer, nomic_core::ToolError> {
        Err(nomic_core::ToolError::new("no answer"))
    }
}

/// 执行 read 并取出文本内容。
async fn read_text(tool: &ReadTool, path: &str) -> String {
    let result = tool
        .execute(
            serde_json::from_value(serde_json::json!({"path": path})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("read");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    text.text.clone()
}

// ── base_dir：workspace 严格归属下的相对路径解析 ─────────────────────────

#[tokio::test]
async fn write_and_read_resolve_relative_to_base_dir() {
    let dir = temp_dir();
    let write = WriteTool::new().with_base_dir(Some(dir.clone()));
    write
        .execute(
            serde_json::from_value(serde_json::json!({"path": "sub/a.txt", "content": "hello"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("write");
    assert_eq!(
        std::fs::read_to_string(dir.join("sub/a.txt")).expect("read back"),
        "hello",
        "相对路径应写入基准目录下"
    );

    let read = ReadTool::new().with_base_dir(Some(dir.clone()));
    let result = read
        .execute(
            serde_json::from_value(serde_json::json!({"path": "sub/a.txt"})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("read");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.contains("hello"));
}

#[tokio::test]
async fn bash_runs_in_base_dir() {
    let dir = temp_dir();
    let result = BashTool::new()
        .with_base_dir(Some(dir.clone()))
        .execute(
            serde_json::from_value(serde_json::json!({"command": "pwd"})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("bash");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    let expected = std::fs::canonicalize(&dir).unwrap_or(dir);
    assert!(
        text.text.contains(&expected.display().to_string()),
        "命令应在基准目录执行：{}",
        text.text
    );
}

#[tokio::test]
async fn grep_and_find_default_root_is_base_dir() {
    let dir = temp_dir();
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").expect("fixture");

    let found = nomic_tools::FindTool::new()
        .with_base_dir(Some(dir.clone()))
        .execute(
            serde_json::from_value(serde_json::json!({"pattern": "*.rs"})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("find");
    let nomic_ai::UserContent::Text(text) = &found.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.contains("main.rs"), "{}", text.text);

    let grep = nomic_tools::GrepTool::new().with_base_dir(Some(dir));
    let matched = grep
        .execute(
            serde_json::from_value(serde_json::json!({"pattern": "fn main"})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("grep");
    let nomic_ai::UserContent::Text(text) = &matched.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.contains("fn main"), "{}", text.text);
}

#[tokio::test]
async fn shared_base_dir_switch_applies_to_next_execution() {
    let dir_a = temp_dir();
    let dir_b = temp_dir();
    std::fs::write(dir_a.join("a.txt"), "in a\n").expect("fixture a");
    std::fs::write(dir_b.join("b.txt"), "in b\n").expect("fixture b");

    let base = nomic_tools::BaseDir::new(Some(dir_a.clone()));
    let read = ReadTool::new().with_shared_base_dir(&base);
    assert!(read_text(&read, "a.txt").await.contains("in a"));
    // 切换到另一个 workspace：同一工具的下一次执行以新基准解析
    base.set(dir_b.clone());
    assert!(read_text(&read, "b.txt").await.contains("in b"));

    // 共享同一构建入口的工具集跟随同一句柄
    let sink = std::sync::Arc::new(NoopSink);
    let tools = nomic_tools::default_tools_in_shared(&base, nomic_tools::TodoStore::new(), sink);
    let write = tools
        .iter()
        .find(|tool| tool.name() == "write")
        .expect("write tool");
    write
        .execute(
            serde_json::from_value(serde_json::json!({"path": "c.txt", "content": "in c"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("write");
    assert_eq!(
        std::fs::read_to_string(dir_b.join("c.txt")).expect("read back"),
        "in c",
        "共享句柄的工具集应写入切换后的 workspace"
    );
}
