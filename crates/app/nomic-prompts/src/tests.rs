// 测试数据大量包含模板占位符字面量（${1:-default} 等），并非格式化参数
#![allow(clippy::literal_string_with_formatting_args)]

use super::*;

fn temp_template(root: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(root).expect("mkdir");
    std::fs::write(root.join(format!("{name}.md")), text).expect("write");
}

fn resolver(cwd: &Path, roots: Vec<(&Path, PromptScope)>) -> PromptResolver {
    PromptResolver::new(
        cwd,
        ProjectDiscovery::Roots(Vec::new()),
        roots
            .into_iter()
            .map(|(path, scope)| PromptRoot {
                path: path.to_path_buf(),
                scope,
            })
            .collect(),
        Vec::new(),
    )
    .expect("resolver")
}

#[test]
fn parses_frontmatter_and_body() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_template(
        tmp.path(),
        "review",
        "---\ndescription: Review staged git changes\nargument-hint: \"<path>\"\n---\nReview the staged changes.\n",
    );
    let template = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)])
        .resolve("review")
        .expect("resolve");
    assert_eq!(template.description, "Review staged git changes");
    assert_eq!(template.argument_hint.as_deref(), Some("<path>"));
    assert_eq!(template.body, "Review the staged changes.");
    assert_eq!(template.scope, PromptScope::Project);
}

#[test]
fn fallback_description_uses_first_non_empty_line() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_template(tmp.path(), "plain", "\n# Do useful work\nBody\n");
    let template = resolver(tmp.path(), vec![(tmp.path(), PromptScope::User)])
        .resolve("plain")
        .expect("resolve");
    assert_eq!(template.description, "Do useful work");
    assert_eq!(template.argument_hint, None);
}

#[test]
fn higher_priority_root_overrides_same_name() {
    let tmp = tempfile::tempdir().expect("tmp");
    let low = tmp.path().join("low");
    let high = tmp.path().join("high");
    temp_template(&low, "shared", "low body");
    temp_template(&high, "shared", "high body");
    let template = resolver(
        tmp.path(),
        vec![
            (low.as_path(), PromptScope::User),
            (high.as_path(), PromptScope::Project),
        ],
    )
    .resolve("shared")
    .expect("resolve");
    assert_eq!(template.body, "high body");
    assert_eq!(template.scope, PromptScope::Project);
}

#[test]
fn explicit_path_overrides_discovered_and_accepts_file_or_dir() {
    let tmp = tempfile::tempdir().expect("tmp");
    let discovered = tmp.path().join("discovered");
    temp_template(&discovered, "shared", "discovered body");
    temp_template(&discovered, "other", "other body");
    let explicit_dir = tmp.path().join("explicit");
    temp_template(&explicit_dir, "shared", "explicit dir body");
    let explicit_file = tmp.path().join("single.md");
    std::fs::write(&explicit_file, "explicit file body").expect("write");
    let resolver = PromptResolver::new(
        tmp.path(),
        ProjectDiscovery::Roots(Vec::new()),
        vec![PromptRoot {
            path: discovered,
            scope: PromptScope::Project,
        }],
        vec![explicit_dir, explicit_file],
    )
    .expect("resolver");
    assert_eq!(
        resolver.resolve("shared").expect("resolve").body,
        "explicit dir body"
    );
    assert_eq!(
        resolver.resolve("shared").expect("resolve").scope,
        PromptScope::Explicit
    );
    assert_eq!(
        resolver.resolve("single").expect("resolve").body,
        "explicit file body"
    );
    assert_eq!(
        resolver.resolve("other").expect("resolve").body,
        "other body"
    );
}

#[test]
fn project_templates_prefer_nearer_dir() {
    let tmp = tempfile::tempdir().expect("tmp");
    let outer = tmp.path().join("outer");
    let inner = outer.join("inner");
    temp_template(&outer.join(".nomic/prompts"), "shared", "outer body");
    temp_template(&inner.join(".nomic/prompts"), "shared", "inner body");
    let resolver = PromptResolver::new(&inner, ProjectDiscovery::Ancestors, Vec::new(), Vec::new())
        .expect("resolver");
    assert_eq!(
        resolver.resolve("shared").expect("resolve").body,
        "inner body"
    );
}

#[test]
fn discovery_is_non_recursive() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_template(tmp.path(), "top", "top body");
    temp_template(&tmp.path().join("nested"), "nested", "nested body");
    let catalog = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)]).catalog();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].name, "top");
}

#[test]
fn broken_template_is_skipped_without_breaking_catalog() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_template(tmp.path(), "good", "good body");
    temp_template(
        tmp.path(),
        "broken",
        "---\nmetadata: {nested: flow}\n---\nbody\n",
    );
    // 非法名称的文件同样只被跳过。
    temp_template(tmp.path(), "BadName", "bad name body");
    // 非 .md 文件不参与发现。
    std::fs::write(tmp.path().join("notes.txt"), "not a template").expect("write");
    let catalog =
        resolver(tmp.path(), vec![(tmp.path(), PromptScope::User)]).catalog_with_diagnostics();
    assert_eq!(catalog.templates.len(), 1);
    assert_eq!(catalog.templates[0].name, "good");
    assert_eq!(catalog.errors.len(), 2);
    assert!(
        catalog
            .errors
            .iter()
            .any(|error| matches!(error, PromptsError::InvalidFrontmatter { .. }))
    );
    assert!(
        catalog
            .errors
            .iter()
            .any(|error| matches!(error, PromptsError::InvalidName { .. }))
    );
}

#[test]
fn rejects_path_traversal_name() {
    let tmp = tempfile::tempdir().expect("tmp");
    let resolver = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)]);
    let error = resolver.resolve("../secret").expect_err("invalid");
    assert!(matches!(error, PromptsError::InvalidName { .. }));
}

#[test]
fn resolve_not_found_lists_available() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_template(tmp.path(), "review", "body");
    let error = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)])
        .resolve("missing")
        .expect_err("not found");
    let PromptsError::NotFound { available, .. } = error else {
        panic!("expected NotFound");
    };
    assert_eq!(available, vec!["review"]);
}

#[test]
fn parses_block_scalar_description_and_nested_unknown_fields() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_template(
        tmp.path(),
        "folded",
        "---\ndescription: >-\n  First line\n  second line\nmetadata:\n  category: test\n---\nBody\n",
    );
    let template = resolver(tmp.path(), vec![(tmp.path(), PromptScope::Project)])
        .resolve("folded")
        .expect("resolve");
    assert_eq!(template.description, "First line second line");
    assert_eq!(template.body, "Body");
}

// ── 参数切分 ────────────────────────────────────────────────────────────

#[test]
fn split_arguments_handles_quotes_and_escapes() {
    assert_eq!(
        split_arguments("Button \"click handler\" 'disabled support'").expect("split"),
        vec!["Button", "click handler", "disabled support"]
    );
    assert_eq!(
        split_arguments("a\\ b \"c\\\"d\" 'e\\f'").expect("split"),
        vec!["a b", "c\"d", "e\\f"]
    );
    assert!(split_arguments("").expect("split").is_empty());
    assert_eq!(
        split_arguments("  \t ").expect("split"),
        Vec::<String>::new()
    );
    // 空引号是一个空参数
    assert_eq!(split_arguments("\"\"").expect("split"), vec![""]);
    let error = split_arguments("\"unterminated").expect_err("unterminated");
    assert!(matches!(error, PromptsError::UnterminatedQuote { .. }));
    assert!(split_arguments("'unterminated").is_err());
}

// ── 模板展开 ────────────────────────────────────────────────────────────

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn expands_positional_and_all_arguments() {
    let body = "Create a component named $1 with features: $@";
    assert_eq!(
        expand_template(
            body,
            &args(&["Button", "onClick handler", "disabled support"])
        ),
        "Create a component named Button with features: Button onClick handler disabled support"
    );
    assert_eq!(
        expand_template("args: $ARGUMENTS", &args(&["a", "b"])),
        "args: a b"
    );
    // 缺失的位置参数展开为空
    assert_eq!(expand_template("<$2>", &args(&["a"])), "<>");
    assert_eq!(expand_template("<$10>", &args(&["a"])), "<>");
}

#[test]
fn expands_defaults() {
    assert_eq!(
        expand_template("Summarize in ${1:-7} bullet points.", &args(&[])),
        "Summarize in 7 bullet points."
    );
    assert_eq!(
        expand_template("Summarize in ${1:-7} bullet points.", &args(&["3"])),
        "Summarize in 3 bullet points."
    );
    // 空参数视为缺失
    assert_eq!(expand_template("${1:-fallback}", &args(&[""])), "fallback");
    assert_eq!(expand_template("${@:-nothing}", &args(&[])), "nothing");
    assert_eq!(expand_template("${ARGUMENTS:-nothing}", &args(&["x"])), "x");
}

#[test]
fn expands_argument_slices() {
    let values = args(&["a", "b", "c", "d"]);
    assert_eq!(expand_template("${@:2}", &values), "b c d");
    assert_eq!(expand_template("${@:2:2}", &values), "b c");
    assert_eq!(expand_template("${@:3:5}", &values), "c d");
    // 越界展开为空
    assert_eq!(expand_template("<${@:9}>", &values), "<>");
    assert_eq!(expand_template("<${@:9:2}>", &values), "<>");
}

#[test]
fn unrecognized_dollar_sequences_stay_literal() {
    let values = args(&["a"]);
    // $5 是合法位置参数（缺失展开为空）；$x / $0 保持字面量
    assert_eq!(
        expand_template("cost is $5 and $x and $0", &values),
        "cost is  and $x and $0"
    );
    // 非法 brace 形式保持字面量
    assert_eq!(
        expand_template("${1:2} ${foo} ${@:0}", &values),
        "${1:2} ${foo} ${@:0}"
    );
    // 结尾孤立的 $
    assert_eq!(expand_template("trailing $", &values), "trailing $");
}

#[test]
fn template_expand_delegates_to_body() {
    let template = PromptTemplate {
        name: "component".to_string(),
        path: PathBuf::from("/tmp/component.md"),
        scope: PromptScope::Project,
        description: "Create a component".to_string(),
        argument_hint: Some("<name>".to_string()),
        body: "Create $1 with $2".to_string(),
    };
    assert_eq!(
        template.expand(&args(&["Button", "hooks"])),
        "Create Button with hooks"
    );
}

#[test]
fn expand_invocation_dispatches_on_slash_prefix() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_template(tmp.path(), "greet", "Hello $1");
    let templates = resolver(tmp.path(), vec![(tmp.path(), PromptScope::User)]).catalog();
    // 非 slash 输入：不处理
    assert_eq!(
        expand_invocation(&templates, "just text").expect("ok"),
        None
    );
    // 空格与冒号两种参数形式
    assert_eq!(
        expand_invocation(&templates, "/greet world").expect("ok"),
        Some("Hello world".to_string())
    );
    assert_eq!(
        expand_invocation(&templates, "/greet:world").expect("ok"),
        Some("Hello world".to_string())
    );
    assert_eq!(
        expand_invocation(&templates, "/greet").expect("ok"),
        Some("Hello ".to_string())
    );
    // 未知名称：NotFound 并列出可用模板
    let error = expand_invocation(&templates, "/missing x").expect_err("not found");
    assert!(matches!(error, PromptsError::NotFound { .. }));
    // 引号未闭合：参数错误
    let error = expand_invocation(&templates, "/greet \"x").expect_err("unterminated");
    assert!(matches!(error, PromptsError::UnterminatedQuote { .. }));
}

#[test]
fn resolver_expand_roundtrip() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_template(tmp.path(), "greet", "Hello $1, from ${2:-nomic}");
    let resolver = resolver(tmp.path(), vec![(tmp.path(), PromptScope::User)]);
    assert_eq!(
        resolver.expand("greet", &args(&["world"])).expect("expand"),
        "Hello world, from nomic"
    );
}
