use super::*;

fn temp_skill(root: &Path, name: &str, text: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("SKILL.md"), text).expect("write");
}

fn resolver(cwd: &Path, roots: Vec<(&Path, SkillScope)>) -> SkillResolver {
    SkillResolver::new(
        cwd,
        ProjectDiscovery::Roots(Vec::new()),
        roots
            .into_iter()
            .map(|(path, scope)| SkillRoot {
                path: path.to_path_buf(),
                scope,
            })
            .collect(),
    )
    .expect("resolver")
}

#[test]
fn parses_frontmatter_and_body() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_skill(
        tmp.path(),
        "rust-review",
        "---\ndescription: Review Rust changes\ntriggers: [rust, review]\n---\n# Steps\nCheck it.\n",
    );
    let skill = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)])
        .resolve("rust-review")
        .expect("resolve");
    assert_eq!(skill.document.description, "Review Rust changes");
    assert_eq!(skill.document.triggers, vec!["rust", "review"]);
    assert_eq!(skill.document.body, "# Steps\nCheck it.");
}

#[test]
fn higher_priority_root_overrides_same_name() {
    let tmp = tempfile::tempdir().expect("tmp");
    let low = tmp.path().join("low");
    let high = tmp.path().join("high");
    temp_skill(&low, "shared", "low body");
    temp_skill(&high, "shared", "high body");
    let skill = resolver(
        tmp.path(),
        vec![
            (low.as_path(), SkillScope::AgentUser),
            (high.as_path(), SkillScope::Project),
        ],
    )
    .resolve("shared")
    .expect("resolve");
    assert_eq!(skill.document.body, "high body");
    assert_eq!(skill.scope, SkillScope::Project);
}

#[test]
fn rejects_path_traversal_name() {
    let tmp = tempfile::tempdir().expect("tmp");
    let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)]);
    let error = resolver.resolve("../secret").expect_err("invalid");
    assert!(matches!(error, SkillsError::InvalidName { .. }));
}

#[test]
fn resolves_skill_sub_resources_with_traversal_guard() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("demo");
    std::fs::create_dir_all(root.join("scripts")).expect("mkdir");
    std::fs::write(root.join("SKILL.md"), "demo body").expect("write");
    std::fs::write(root.join("scripts/run.sh"), "#!/bin/sh\n").expect("write");
    std::fs::write(tmp.path().join("secret.txt"), "secret").expect("write");
    let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)]);

    // 无子路径 / 空子路径：返回 skill 正文指令
    for rel in [None, Some(""), Some(".")] {
        let resource = resolver
            .resolve_resource("demo", rel)
            .expect("instructions");
        assert!(matches!(resource, SkillResource::Instructions(_)));
    }

    // 文件子资源：返回规范化后的绝对路径
    let resource = resolver
        .resolve_resource("demo", Some("scripts//run.sh"))
        .expect("file");
    let SkillResource::File { path, .. } = resource else {
        panic!("expected file resource");
    };
    assert_eq!(path, root.join("scripts/run.sh"));

    // 目录子资源
    let resource = resolver
        .resolve_resource("demo", Some("scripts"))
        .expect("dir");
    assert!(matches!(resource, SkillResource::Directory { .. }));

    // 穿越到 skill 根之外：拒绝（含经中间目录折返的情形）
    for rel in ["../secret.txt", "scripts/../../secret.txt", "/etc/passwd"] {
        let error = resolver
            .resolve_resource("demo", Some(rel))
            .expect_err("traversal");
        assert!(matches!(error, SkillsError::InvalidResourcePath { .. }));
    }

    // 根内不存在的路径
    let error = resolver
        .resolve_resource("demo", Some("scripts/missing.sh"))
        .expect_err("missing");
    assert!(matches!(error, SkillsError::ResourceNotFound { .. }));
}

#[test]
fn fallback_description_uses_first_heading() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_skill(tmp.path(), "plain", "\n# Do useful work\nBody\n");
    let skill = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)])
        .resolve("plain")
        .expect("resolve");
    assert_eq!(skill.document.description, "Do useful work");
}

#[test]
fn project_skill_overrides_user_skill() {
    let tmp = tempfile::tempdir().expect("tmp");
    let project = tmp.path().join("project");
    let user = tmp.path().join("user");
    temp_skill(&project.join(".nomic/skills"), "shared", "project body");
    temp_skill(&user, "shared", "user body");
    let resolver = SkillResolver::new(
        &project,
        ProjectDiscovery::Roots(vec![project.clone()]),
        vec![SkillRoot {
            path: user,
            scope: SkillScope::NomicUser,
        }],
    )
    .expect("resolver");
    let skill = resolver.resolve("shared").expect("resolve");
    assert_eq!(skill.document.body, "project body");
    assert_eq!(skill.scope, SkillScope::Project);
}

#[test]
fn ancestors_mode_prefers_nearer_dir_and_nomic_dir() {
    let tmp = tempfile::tempdir().expect("tmp");
    let outer = tmp.path().join("outer");
    let inner = outer.join("inner");
    temp_skill(&outer.join(".agents/skills"), "shared", "outer agents");
    temp_skill(&outer.join(".nomic/skills"), "shared", "outer nomic");
    temp_skill(&inner.join(".agents/skills"), "shared", "inner agents");
    temp_skill(&inner.join(".nomic/skills"), "shared", "inner nomic");
    // 只在父级 .agents 与 .nomic 中同时存在：同层 .nomic 应优先。
    temp_skill(&outer.join(".agents/skills"), "outer-only", "outer agents");
    temp_skill(&outer.join(".nomic/skills"), "outer-only", "outer nomic");
    let resolver =
        SkillResolver::new(&inner, ProjectDiscovery::Ancestors, Vec::new()).expect("resolver");
    assert_eq!(
        resolver.resolve("shared").expect("resolve").document.body,
        "inner nomic"
    );
    assert_eq!(
        resolver
            .resolve("outer-only")
            .expect("resolve")
            .document
            .body,
        "outer nomic"
    );
}

#[test]
fn roots_mode_prefers_later_root_and_nomic_dir() {
    let tmp = tempfile::tempdir().expect("tmp");
    let low = tmp.path().join("low");
    let high = tmp.path().join("high");
    temp_skill(&low.join(".nomic/skills"), "shared", "low nomic");
    temp_skill(&high.join(".agents/skills"), "shared", "high agents");
    temp_skill(&high.join(".nomic/skills"), "shared", "high nomic");
    let resolver = SkillResolver::new(
        tmp.path(),
        ProjectDiscovery::Roots(vec![low, high]),
        Vec::new(),
    )
    .expect("resolver");
    assert_eq!(
        resolver.resolve("shared").expect("resolve").document.body,
        "high nomic"
    );
}

#[test]
fn parses_block_scalar_description_and_nested_unknown_fields() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_skill(
        tmp.path(),
        "folded",
        "---\nname: folded\ndescription: >-\n  First line\n  second line\nlicense: MIT\nmetadata:\n  category: test\n  version: \"1.0\"\n---\nBody\n",
    );
    temp_skill(
        tmp.path(),
        "literal",
        "---\ndescription: |\n  line one\n  line two\n---\nBody\n",
    );
    let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)]);
    let folded = resolver.resolve("folded").expect("resolve folded");
    assert_eq!(folded.document.description, "First line second line");
    assert_eq!(folded.document.body, "Body");
    let literal = resolver.resolve("literal").expect("resolve literal");
    assert_eq!(literal.document.description, "line one\nline two");
}

#[test]
fn active_skill_tag_roundtrips_and_rejects_plain_text() {
    let skill = ActivatedSkill {
        name: "rust-review".to_string(),
        scope: SkillScope::Project,
        path: PathBuf::from("/repo/.nomic/skills/rust-review/SKILL.md"),
        root: PathBuf::from("/repo/.nomic/skills/rust-review"),
        instructions: "# Review\nCheck unsafe code.".to_string(),
    };
    let tag = skill.prompt_tag();
    assert!(tag.starts_with(
        "<active_skill name=\"rust-review\" scope=\"project\" \
         path=\"/repo/.nomic/skills/rust-review/SKILL.md\">"
    ));
    // 注入块尾部带 skill 根目录指引：相对路径的解析基准 + 子资源读取方式
    assert!(tag.ends_with(
        "# Review\nCheck unsafe code.\n\n\
         [Skill directory: /repo/.nomic/skills/rust-review — relative paths referenced \
         by this skill resolve against this directory; read its files via \
         skill://rust-review/<path> or the filesystem, and run its scripts with bash, \
         as needed.]\n</active_skill>"
    ));

    // 标签后允许拼接其他文本（如会话内注入的说明）。
    let parsed = parse_active_skill_tag(&format!("{tag}\n\nmanual note")).expect("parse");
    assert_eq!(parsed.name, "rust-review");
    assert_eq!(parsed.scope, Some(SkillScope::Project));
    assert_eq!(
        parsed.path,
        Some(PathBuf::from("/repo/.nomic/skills/rust-review/SKILL.md"))
    );

    // 旧格式缺 scope / path 时仍可解析出 name。
    let legacy = parse_active_skill_tag("<active_skill name=\"legacy\">\nbody").expect("legacy");
    assert_eq!(legacy.name, "legacy");
    assert_eq!(legacy.scope, None);
    assert_eq!(legacy.path, None);

    assert!(parse_active_skill_tag("plain text").is_none());
    assert!(parse_active_skill_tag("<active_skill scope=\"project\">").is_none());
    assert!("garbage".parse::<SkillScope>().is_err());
}

#[test]
fn frontmatter_enabled_and_hide_control_visibility() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_skill(tmp.path(), "normal", "normal body");
    temp_skill(tmp.path(), "off", "---\nenabled: false\n---\noff body");
    temp_skill(tmp.path(), "hidden", "---\nhide: true\n---\nhidden body");
    let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::Project)]);

    // enabled: false —— 彻底跳过，resolve 也找不到
    let error = resolver.resolve("off").expect_err("disabled");
    assert!(matches!(error, SkillsError::NotFound { .. }));

    // hide: true —— 可 resolve / 激活，但不出现在 prompt 清单
    assert_eq!(
        resolver
            .resolve("hidden")
            .expect("resolve hidden")
            .document
            .body,
        "hidden body"
    );
    let prompt = resolver.prompt_catalog().expect("non-empty");
    assert!(prompt.contains("skill://normal"));
    assert!(!prompt.contains("hidden"));
    assert!(!prompt.contains("skill://off"));

    // 非布尔值：frontmatter 非法，skill 被跳过并记录诊断
    temp_skill(tmp.path(), "bad-bool", "---\nenabled: maybe\n---\nbody");
    let catalog = resolver.catalog_with_diagnostics();
    assert!(catalog.skills.iter().all(|skill| skill.name != "bad-bool"));
    assert!(
        catalog
            .errors
            .iter()
            .any(|error| matches!(error, SkillsError::InvalidFrontmatter { .. }))
    );
}

#[test]
fn broken_skill_is_skipped_without_breaking_catalog() {
    let tmp = tempfile::tempdir().expect("tmp");
    temp_skill(tmp.path(), "good", "good body");
    temp_skill(
        tmp.path(),
        "broken",
        "---\nmetadata: {nested: flow}\n---\nbody\n",
    );
    // 非法名称的目录同样只被跳过。
    temp_skill(tmp.path(), "BadName", "bad name body");
    let resolver = resolver(tmp.path(), vec![(tmp.path(), SkillScope::AgentUser)]);

    let catalog = resolver.catalog_with_diagnostics();
    assert_eq!(catalog.skills.len(), 1);
    assert_eq!(catalog.skills[0].name, "good");
    assert_eq!(catalog.errors.len(), 2);
    assert!(
        catalog
            .errors
            .iter()
            .any(|error| matches!(error, SkillsError::InvalidFrontmatter { .. }))
    );
    assert!(
        catalog
            .errors
            .iter()
            .any(|error| matches!(error, SkillsError::InvalidName { .. }))
    );

    // resolve / prompt_catalog 均不受坏 skill 影响。
    assert_eq!(
        resolver.resolve("good").expect("resolve").document.body,
        "good body"
    );
    let prompt = resolver.prompt_catalog().expect("non-empty");
    assert!(prompt.contains("skill://good"));
    assert!(!prompt.contains("broken"));
    // 清单头部说明子资源读取方式
    assert!(prompt.contains("skill://<name>/<path>"));
}
