//! 内部 URI（ADR-0040 / ADR-0042 VFS 化）与 read/write/edit 的集成测试：
//! immutable 闸、可写协议读-改-写、`:conflicts` 选择器、grep/bash 对齐。

use nomic_core::{AgentTool, ToolUpdateCallback};
use nomic_skills::{ProjectDiscovery, SkillResolver, SkillRoot, SkillScope};
use nomic_tools::{EditTool, ReadTool, WriteTool};
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

// ── T5：write/edit 的 URI immutable 闸 ─────────────────────────────────────

/// 测试用可写内存 VFS（local:// 之外的虚拟协议验证）。
struct MemVfs {
    store: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[async_trait::async_trait]
impl nomic_vfs::Vfs for MemVfs {
    fn scheme(&self) -> &'static str {
        "mem"
    }
    fn capabilities(&self) -> nomic_vfs::VfsCapabilities {
        nomic_vfs::VfsCapabilities {
            writable: true,
            immutable: false,
            completion: false,
        }
    }
    async fn stat(
        &self,
        uri: &nomic_vfs::InternalUri,
    ) -> Result<nomic_vfs::VfsMetadata, nomic_vfs::VfsError> {
        let key = uri.without_query();
        let content = self
            .store
            .lock()
            .expect("lock")
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let mut meta = nomic_vfs::VfsMetadata::file(nomic_vfs::ContentType::Plain, None);
        meta.size = Some(content.len());
        Ok(meta)
    }
    async fn read(
        &self,
        uri: &nomic_vfs::InternalUri,
    ) -> Result<nomic_vfs::VfsFile, nomic_vfs::VfsError> {
        // 以不含 query 的 href 为键（query 是选择器参数，非资源标识）
        let key = uri.without_query();
        let content = self
            .store
            .lock()
            .expect("lock")
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let meta = self.stat(uri).await?;
        Ok(nomic_vfs::VfsFile::text(key, content, meta))
    }
    async fn write(
        &self,
        uri: &nomic_vfs::InternalUri,
        content: &str,
    ) -> Result<(), nomic_vfs::VfsError> {
        self.store
            .lock()
            .expect("lock")
            .insert(uri.raw_href.clone(), content.to_string());
        Ok(())
    }
}

fn mem_router() -> (
    std::sync::Arc<nomic_vfs::VfsRouter>,
    std::sync::Arc<MemVfs>,
) {
    let mem = std::sync::Arc::new(MemVfs {
        store: std::sync::Mutex::new(std::collections::HashMap::new()),
    });
    let mut router = nomic_vfs::VfsRouter::new();
    router.mount(mem.clone());
    (std::sync::Arc::new(router), mem)
}

fn skill_router(dir: &std::path::Path) -> std::sync::Arc<nomic_vfs::VfsRouter> {
    let skills_dir = dir.join("skills");
    let demo = skills_dir.join("demo");
    std::fs::create_dir_all(&demo).expect("skill dir");
    std::fs::write(demo.join("SKILL.md"), "demo body").expect("write skill");
    let resolver = SkillResolver::new(
        dir,
        ProjectDiscovery::Roots(Vec::new()),
        vec![SkillRoot {
            path: skills_dir,
            scope: SkillScope::Project,
        }],
    )
    .expect("resolver");
    let mut router = nomic_vfs::VfsRouter::new();
    router.mount(std::sync::Arc::new(nomic_vfs::fs::SkillVfs::new(resolver)));
    std::sync::Arc::new(router)
}

#[tokio::test]
async fn write_and_edit_reject_immutable_skill_uri() {
    let dir = temp_dir();
    let router = skill_router(&dir);

    let error = WriteTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(
                serde_json::json!({"path": "skill://demo", "content": "hacked"}),
            )
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only"), "{error}");

    let error = EditTool::new()
        .with_vfs_router(router)
        .execute(
            serde_json::from_value(serde_json::json!({
                "path": "skill://demo",
                "edits": [{"oldText": "demo", "newText": "hacked"}],
            }))
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only"), "{error}");
}

#[tokio::test]
async fn write_rejects_unknown_scheme_and_selectors() {
    let (router, _mem) = mem_router();

    let error = WriteTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(serde_json::json!({"path": "bogus://x", "content": "data"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("Unknown protocol: bogus://"), "{message}");
    assert!(message.contains("mem://"), "{message}");

    // 尾挂选择器不是合法写入目标
    let error = WriteTool::new()
        .with_vfs_router(router)
        .execute(
            serde_json::from_value(serde_json::json!({"path": "mem://a:1-2", "content": "data"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("not valid write targets"),
        "{error}"
    );
}

#[tokio::test]
async fn write_and_edit_roundtrip_writable_uri() {
    let (router, mem) = mem_router();

    let result = WriteTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(
                serde_json::json!({"path": "mem://doc", "content": "alpha\nbeta\n"}),
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
    assert!(text.text.contains("Successfully wrote"), "{}", text.text);
    assert_eq!(
        mem.store
            .lock()
            .expect("lock")
            .get("mem://doc")
            .map(String::as_str),
        Some("alpha\nbeta\n")
    );

    let result = EditTool::new()
        .with_vfs_router(router)
        .execute(
            serde_json::from_value(serde_json::json!({
                "path": "mem://doc",
                "edits": [{"oldText": "beta", "newText": "BETA"}],
            }))
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("edit");
    assert_eq!(
        mem.store
            .lock()
            .expect("lock")
            .get("mem://doc")
            .map(String::as_str),
        Some("alpha\nBETA\n")
    );
    let details = result.details.expect("details");
    assert!(details["diff"].as_str().expect("diff").contains("-beta"));
}

// ── T7：:conflicts 选择器 ──────────────────────────────────────────────────

#[tokio::test]
async fn read_conflicts_selector_returns_conflict_regions() {
    let dir = temp_dir();
    let base = dir.join("base.md");
    let theirs = dir.join("theirs.md");
    std::fs::write(&base, "a\nb\nc\n").expect("write base");
    std::fs::write(&theirs, "a\nB-theirs\nc\n").expect("write theirs");

    let (router, mem) = mem_router();
    mem.store
        .lock()
        .expect("lock")
        .insert("mem://ours".to_string(), "a\nB-ours\nc\n".to_string());

    let target = format!(
        "mem://ours:conflicts?base={}&theirs={}",
        base.display(),
        theirs.display()
    );
    let result = ReadTool::with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(serde_json::json!({"path": target})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("read conflicts");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(text.text, "B-ours");
    let details = result.details.expect("details");
    assert_eq!(
        details["conflicts"].as_array().expect("conflicts")[0],
        serde_json::json!([2, 2, false])
    );

    // 无冲突：ours 改第 2 行、theirs 改第 4 行（有间隔上下文 → 干净合并）
    let base_wide = dir.join("base-wide.md");
    let theirs_clean = dir.join("theirs-clean.md");
    std::fs::write(&base_wide, "a\nb\nc\nd\ne\n").expect("write base");
    std::fs::write(&theirs_clean, "a\nb\nc\nD\ne\n").expect("write theirs");
    mem.store
        .lock()
        .expect("lock")
        .insert("mem://clean".to_string(), "a\nB\nc\nd\ne\n".to_string());
    let target = format!(
        "mem://clean:conflicts?base={}&theirs={}",
        base_wide.display(),
        theirs_clean.display()
    );
    let result = ReadTool::with_vfs_router(router)
        .execute(
            serde_json::from_value(serde_json::json!({"path": target})).expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("read clean");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert_eq!(text.text, "No conflicts found in mem://clean.");
}

#[tokio::test]
async fn read_conflicts_selector_requires_theirs() {
    let (router, _mem) = mem_router();
    let error = ReadTool::with_vfs_router(router)
        .execute(
            serde_json::from_value(serde_json::json!({"path": "mem://ours:conflicts"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("?theirs=<path>"), "{error}");
}

// ── T9：local:// 工作区协议 ────────────────────────────────────────────────

fn local_router(dir: &std::path::Path) -> std::sync::Arc<nomic_vfs::VfsRouter> {
    let mut router = nomic_vfs::VfsRouter::new();
    router.mount(std::sync::Arc::new(nomic_vfs::fs::LocalVfs::new(
        nomic_vfs::WorkspaceRoot::new(Some(dir.to_path_buf())),
    )));
    std::sync::Arc::new(router)
}

#[tokio::test]
async fn local_uri_read_write_edit_roundtrip() {
    let dir = temp_dir();
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write");
    let router = local_router(&dir);

    // read：文件与目录清单
    let result = ReadTool::with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(serde_json::json!({"path": "local://main.rs:1"}))
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
        "fn main() {}\n\n[1 more lines in file. Use offset=2 to continue.]"
    );

    // stat/list：纯元数据与类型化条目（不整读内容）
    let meta = router.stat("local://main.rs").await.expect("stat");
    assert_eq!(meta.kind, nomic_vfs::VfsKind::File);
    assert!(!meta.is_immutable());
    let meta = router.stat("local://").await.expect("stat root");
    assert_eq!(meta.kind, nomic_vfs::VfsKind::Directory);
    assert!(meta.is_immutable()); // 目录清单由 router 盖不可变章
    let entries = router.list("local://").await.expect("list");
    assert!(
        entries
            .iter()
            .any(|e| e.name == "main.rs" && e.kind == nomic_vfs::VfsKind::File),
        "{entries:?}"
    );

    // write：新建（含父目录创建）
    WriteTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(
                serde_json::json!({"path": "local://notes/new.md", "content": "hello\n"}),
            )
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("write");
    assert_eq!(
        std::fs::read_to_string(dir.join("notes/new.md")).expect("fs read"),
        "hello\n"
    );

    // edit：读-改-写
    EditTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(serde_json::json!({
                "path": "local://main.rs",
                "edits": [{"oldText": "main", "newText": "run"}],
            }))
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("edit");
    assert_eq!(
        std::fs::read_to_string(dir.join("main.rs")).expect("fs read"),
        "fn run() {}\n"
    );

    // 目录清单是派生内容：不可编辑
    let error = EditTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(serde_json::json!({
                "path": "local://notes",
                "edits": [{"oldText": "x", "newText": "y"}],
            }))
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only"), "{error}");

    // 越出 workspace 根：拒绝
    let error = ReadTool::with_vfs_router(router)
        .execute(
            serde_json::from_value(serde_json::json!({"path": "local://../escape"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("escapes the workspace root"),
        "{error}"
    );
}

// ── T12：grep/bash 的 URI 对齐 ─────────────────────────────────────────────

#[tokio::test]
async fn grep_accepts_uri_search_root() {
    let dir = temp_dir();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").expect("write");
    std::fs::write(dir.join("other.rs"), "// nothing\n").expect("write");
    let router = local_router(&dir);

    let result = nomic_tools::GrepTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(serde_json::json!({"pattern": "main", "path": "local://src"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("grep");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(text.text.contains("main.rs:1:"), "{}", text.text);

    // 选择器对 grep 无语义：明确报错
    let error = nomic_tools::GrepTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(
                serde_json::json!({"pattern": "main", "path": "local://src:1-2"}),
            )
            .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no meaning for grep"), "{error}");

    // 虚拟资源（无底层路径）报错
    let (mem_only, _mem) = mem_router();
    let error = nomic_tools::GrepTool::new()
        .with_vfs_router(mem_only)
        .execute(
            serde_json::from_value(serde_json::json!({"pattern": "x", "path": "mem://doc"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("virtual resource"), "{error}");
}

#[tokio::test]
async fn bash_cd_rewrites_uri_to_source_path() {
    let dir = temp_dir();
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    let router = local_router(&dir);

    let result = nomic_tools::BashTool::new()
        .with_vfs_router(router.clone())
        .execute(
            serde_json::from_value(serde_json::json!({"command": "cd local://src && pwd"}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("bash");
    let nomic_ai::UserContent::Text(text) = &result.content[0] else {
        panic!("expected text")
    };
    assert!(
        text.text.contains(&dir.join("src").display().to_string()),
        "{}",
        text.text
    );

    // 裸 `cd <uri>` 单一命令同样重写
    let result = nomic_tools::BashTool::new()
        .with_vfs_router(router)
        .execute(
            serde_json::from_value(serde_json::json!({"command": "cd local://src", "timeout": 5}))
                .expect("params"),
            CancellationToken::new(),
            no_update(),
        )
        .await
        .expect("bash cd");
    let _ = result;
}
