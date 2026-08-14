//! `@` mention 补全与展示折叠相关测试。

use super::*;

fn skill_entry(name: &str) -> SkillEntry {
    SkillEntry {
        name: name.to_string(),
        description: format!("{name} 描述"),
        scope: SkillScope::Project,
    }
}

#[test]
fn mention_type_completion_and_tab() {
    let mut app = app();
    app.input_mut()
        .set_available_skills(vec![skill_entry("jujutsu"), skill_entry("rust-review")]);

    app.paste_text("@");
    let mention = app.input().mention().expect("`@` 弹出类型候选");
    let fragments: Vec<&str> = mention
        .candidates
        .iter()
        .map(|c| c.fragment.as_str())
        .collect();
    assert_eq!(fragments, vec!["@skill:", "@file:"]);

    app.press(Key::Tab);
    assert_eq!(app.input().text(), "@skill:");
}

#[test]
fn mention_skill_completion_filters_and_completes() {
    let mut app = app();
    app.input_mut()
        .set_available_skills(vec![skill_entry("jujutsu"), skill_entry("rust-review")]);

    app.paste_text("@skill:ju");
    let mention = app.input().mention().expect("skill 候选");
    assert_eq!(mention.candidates.len(), 1);
    assert_eq!(mention.candidates[0].fragment, "@skill:jujutsu");

    app.press(Key::Tab);
    assert_eq!(app.input().text(), "@skill:jujutsu");
}

#[test]
fn mention_esc_dismisses_then_normal() {
    let mut app = app();
    app.input_mut()
        .set_available_skills(vec![skill_entry("jujutsu")]);

    app.paste_text("@skill:");
    assert!(app.input().mention().is_some());

    app.press(Key::Esc);
    assert!(app.input().mention().is_none());
    assert_eq!(app.mode(), Mode::Insert);

    app.press(Key::Esc);
    assert_eq!(app.mode(), Mode::Normal);
}

#[test]
fn mention_up_down_selects_candidate() {
    let mut app = app();
    app.input_mut()
        .set_available_skills(vec![skill_entry("alpha"), skill_entry("beta")]);

    app.paste_text("@skill:");
    app.press(Key::Down);
    let mention = app.input().mention().expect("mention");
    assert_eq!(mention.selected, 1);
}

#[test]
fn mention_unknown_prefix_shows_no_popup() {
    let mut app = app();
    app.input_mut()
        .set_available_skills(vec![skill_entry("jujutsu")]);

    app.paste_text("@unknown");
    assert!(app.input().mention().is_none());
}

#[test]
fn chat_collapses_mention_blocks() {
    let mut app = app();
    let text = "参考 <active_skill name=\"jujutsu\" scope=\"project\" path=\"/x/SKILL.md\">\nbody\n[Skill directory: /x]\n</active_skill> 与 <file path=\"/x/notes.txt\">\n内容\n</file> 一起";
    app.handle_event(&AgentEvent::MessageStart(user_message(text)));

    let ChatItem::User(displayed) = &app.chat().items()[0] else {
        panic!("应为折叠后的 user 条目")
    };
    assert!(displayed.contains("@skill:jujutsu"), "{displayed}");
    assert!(displayed.contains("@file:/x/notes.txt"), "{displayed}");
    assert!(!displayed.contains("body"), "{displayed}");
    assert!(!displayed.contains("内容"), "{displayed}");
}
