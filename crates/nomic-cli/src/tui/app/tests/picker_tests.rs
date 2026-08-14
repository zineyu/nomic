//! picker / 模型 / tree / help 相关测试。

use super::*;

/// NORMAL：A/I 分别到输入末尾/行首回 INSERT（草稿编辑统一回 INSERT）。
#[test]
fn normal_a_i_return_to_insert_at_edges() {
    let mut app = app();
    app.paste_text("hello world foo");
    app.press(Key::Home);
    app.press(Key::Esc);
    assert_eq!(app.mode(), Mode::Normal);

    app.press(Key::Char('A'));
    assert_eq!(app.mode(), Mode::Insert);
    assert_eq!(app.input.cursor_position().1, 15);

    app.press(Key::Esc);
    app.press(Key::Char('I'));
    assert_eq!(app.mode(), Mode::Insert);
    assert_eq!(app.input.cursor_position().1, 0);
}

/// picker 过滤（fzf 风格）：可打印字符即过滤，选中随过滤对齐可选行；
/// Backspace 逐字删除，Esc 先清过滤再关闭；j/k/q 同样是过滤字符。
#[test]
fn picker_filter_narrows_and_esc_clears_first() {
    let rows = || {
        vec![
            PickerRow {
                selectable: true,
                id: "s1".to_string(),
                text: "alpha session".to_string(),
            },
            PickerRow {
                selectable: true,
                id: "s2".to_string(),
                text: "beta session".to_string(),
            },
            PickerRow {
                selectable: true,
                id: "s3".to_string(),
                text: "beta branch".to_string(),
            },
        ]
    };
    let mut app = app();
    app.open_resume_picker(rows());

    // 输入即过滤（大小写不敏感子串）
    for c in "BETA".chars() {
        app.press(Key::Char(c));
    }
    let picker = app.picker().expect("picker");
    assert_eq!(picker.visible(), vec![1, 2]);
    assert_eq!(picker.core.selected, 0);

    // ↓ 在过滤结果上移动，Enter 确认命中行
    app.press(Key::Down);
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::Resume(id)] if id == "s3"));
    assert!(app.picker().is_none());

    // Esc 先清过滤、再关闭
    app.open_resume_picker(rows());
    app.press(Key::Char('x'));
    assert_eq!(app.picker().expect("picker").core.filter, "x");
    assert!(app.press(Key::Esc).is_empty());
    assert!(app.picker().is_some(), "第一次 Esc 只清过滤");
    assert_eq!(app.picker().expect("picker").visible().len(), 3);
    assert!(app.press(Key::Esc).is_empty());
    assert!(app.picker().is_none(), "第二次 Esc 关闭 picker");

    // 无匹配行：Enter 不确认、保持打开
    app.open_resume_picker(rows());
    for c in "zzz".chars() {
        app.press(Key::Char(c));
    }
    assert!(app.picker().expect("picker").visible().is_empty());
    assert!(app.press(Key::Enter).is_empty());
    assert!(app.picker().is_some());
}

/// picker 的 Home/End 与半页翻：跳首/尾并对齐可选行。
#[test]
fn picker_home_end_and_half_page() {
    let rows: Vec<PickerRow> = (0..30)
        .map(|i| PickerRow {
            selectable: true,
            id: format!("s{i}"),
            text: format!("session {i}"),
        })
        .collect();
    let mut app = app();
    app.open_resume_picker(rows);

    app.press(Key::End);
    assert_eq!(app.picker().expect("picker").core.selected, 29);
    app.press(Key::Home);
    assert_eq!(app.picker().expect("picker").core.selected, 0);
    app.press(Key::Ctrl('d'));
    assert_eq!(app.picker().expect("picker").core.selected, 10);
    app.press(Key::Ctrl('u'));
    assert_eq!(app.picker().expect("picker").core.selected, 0);

    // g/G 普通过滤字符（不过滤语言引入序列键，一键一义）
    app.press(Key::Char('g'));
    assert_eq!(app.picker().expect("picker").core.filter, "g");
}

/// `models` 解析：无参打开选择器，带 id（空格或冒号）直接切换，
/// id 含空白报用法错误。
#[test]
fn parse_models_forms() {
    assert_eq!(
        parse_slash("models"),
        SlashParse::Known(SlashAction::Models(None))
    );
    assert_eq!(
        parse_slash("models:gpt-5.2"),
        SlashParse::Known(SlashAction::Models(Some("gpt-5.2".to_string())))
    );
    assert_eq!(
        parse_slash("models gpt-5.2"),
        SlashParse::Known(SlashAction::Models(Some("gpt-5.2".to_string())))
    );
    assert!(matches!(
        parse_slash("models a b"),
        SlashParse::InvalidUsage(_)
    ));
    assert_eq!(
        parse_slash("modelsx"),
        SlashParse::Unknown("modelsx".to_string())
    );
}

/// 思考级别选择器（模型切换流程第二步）：Enter 产出 SetReasoning 效果，
/// Esc 产出 CancelModelSwitch 效果并关闭选择器。
#[test]
fn reasoning_picker_enter_sets_level_esc_aborts_switch() {
    let mut app = app();
    let rows = || {
        vec![
            PickerRow {
                selectable: true,
                id: "off".to_string(),
                text: "off row".to_string(),
            },
            PickerRow {
                selectable: true,
                id: "high".to_string(),
                text: "high row".to_string(),
            },
        ]
    };
    app.open_reasoning_picker(rows(), 1);
    assert_eq!(app.picker().expect("picker").core.selected, 1);
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::SetReasoning(id)] if id == "high"));
    assert!(app.picker().is_none());

    app.open_reasoning_picker(rows(), 0);
    let effects = app.press(Key::Esc);
    assert!(matches!(&effects[..], [Effect::CancelModelSwitch]));
    assert!(app.picker().is_none());
    // 其他选择器 Esc 不产生取消效果
    app.open_model_picker(
        vec![PickerRow {
            selectable: true,
            id: "m".to_string(),
            text: "m row".to_string(),
        }],
        0,
    );
    assert!(app.press(Key::Esc).is_empty());
    assert!(app.picker().is_none());
}

/// `models` 选择器：预选中当前模型，Enter 产出 SwitchModel 效果。
#[test]
fn model_picker_enter_returns_switch_effect() {
    let mut app = app();
    app.open_model_picker(
        vec![
            PickerRow {
                selectable: true,
                id: "m1".to_string(),
                text: "m1 row".to_string(),
            },
            PickerRow {
                selectable: true,
                id: "m2".to_string(),
                text: "m2 row".to_string(),
            },
        ],
        1,
    );
    assert_eq!(app.picker().expect("picker").core.selected, 1);
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::SwitchModel(id)] if id == "m2"));
    assert!(app.picker().is_none());
}

/// `models` 无参 → ListModels 效果；切换成功后状态栏模型信息更新。
#[test]
fn models_slash_effects_and_set_model_updates_status() {
    let mut app = app();
    open_command(&mut app, "models");
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::ListModels]));

    open_command(&mut app, "models:gpt-5.2");
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::SwitchModel(id)] if id == "gpt-5.2"));

    app.set_model("GPT-5.2".to_string(), 400_000);
    assert_eq!(app.model_name(), "GPT-5.2");
    assert_eq!(app.context_window(), 400_000);
}

#[test]
fn unknown_and_invalid_slash_warn_via_notice() {
    let mut unknown = app();
    open_command(&mut unknown, "foobar");
    assert!(unknown.press(Key::Enter).is_empty());
    assert!(unknown.notice().is_some_and(|n| n.contains("未知命令")));
    assert_eq!(unknown.mode(), Mode::Command, "被拒绝时留在命令行");

    let mut invalid = app();
    open_command(&mut invalid, "skill a b");
    assert!(invalid.press(Key::Enter).is_empty());
    assert!(invalid.notice().is_some_and(|n| n.contains("用法")));
    assert_eq!(invalid.mode(), Mode::Command, "被拒绝时留在命令行");
}

#[test]
fn finish_run_clears_running_and_sets_notice() {
    let mut app = app();
    app.handle_event(&AgentEvent::AgentStart);
    app.finish_run(Some("boom".to_string()));
    assert!(!app.is_running());
    assert_eq!(app.notice(), Some("boom"));
    app.finish_run(None);
    assert_eq!(app.notice(), None);
}

#[test]
fn restore_conversation_replaces_items_and_session() {
    let mut app = app();
    app.chat.push_system("旧内容");
    app.restore_conversation(&[*user_message("恢复的")], "sid-1".to_string());
    assert_eq!(app.chat.items().len(), 1);
    assert!(matches!(&app.chat.items()[0], ChatItem::User(t) if t == "恢复的"));
    assert_eq!(app.session_id(), Some("sid-1"));
}

/// `tree` 解析：无参命令；带参数报用法错误。
#[test]
fn parse_tree_forms() {
    assert_eq!(parse_slash("tree"), SlashParse::Known(SlashAction::Tree));
    assert!(matches!(parse_slash("tree x"), SlashParse::InvalidUsage(_)));
    assert!(matches!(
        parse_slash("tree:abc"),
        SlashParse::InvalidUsage(_)
    ));
    assert_eq!(
        parse_slash("treex"),
        SlashParse::Unknown("treex".to_string())
    );
}

/// `tree` 提交 → ListTree 效果。
#[test]
fn tree_slash_produces_list_tree_effect() {
    let mut app = app();
    open_command(&mut app, "tree");
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::ListTree]));
}

/// `tree` 选择器：移动跳过不可选行（工具调用条目），Enter 产出
/// BranchTo 效果。
#[test]
fn tree_picker_skips_unselectable_rows() {
    let rows = vec![
        PickerRow {
            selectable: true,
            id: "user-1".to_string(),
            text: "用户 row".to_string(),
        },
        PickerRow {
            selectable: false,
            id: "tool-1".to_string(),
            text: "工具 row".to_string(),
        },
        PickerRow {
            selectable: true,
            id: "user-2".to_string(),
            text: "用户 row 2".to_string(),
        },
    ];
    let mut app = app();
    app.open_tree_picker(rows, 0);

    // 下移跳过不可选行，直接落在下一个可选行
    assert!(app.press(Key::Down).is_empty());
    assert_eq!(app.picker().expect("picker").core.selected, 2);
    // 上移同样跳过
    assert!(app.press(Key::Up).is_empty());
    assert_eq!(app.picker().expect("picker").core.selected, 0);

    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::BranchTo(id)] if id == "user-1"));
    assert!(app.picker().is_none());
}

/// 末尾是不可选行时，下移到边界不离开最后一个可选行。
#[test]
fn tree_picker_stays_on_last_selectable_at_boundary() {
    let rows = vec![
        PickerRow {
            selectable: true,
            id: "user-1".to_string(),
            text: "用户 row".to_string(),
        },
        PickerRow {
            selectable: false,
            id: "tool-1".to_string(),
            text: "工具 row".to_string(),
        },
    ];
    let mut app = app();
    app.open_tree_picker(rows, 0);

    assert!(app.press(Key::Char('j')).is_empty());
    assert_eq!(app.picker().expect("picker").core.selected, 0);
}

/// 分支切换：以重放的消息替换聊天区，session 不变。
#[test]
fn restore_branch_replaces_items_keeps_session() {
    let mut app = app();
    app.set_session("sid-1".to_string());
    app.chat.push_system("旧内容");
    app.restore_branch(&[*user_message("分支起点")]);
    assert_eq!(app.chat.items().len(), 1);
    assert!(matches!(&app.chat.items()[0], ChatItem::User(t) if t == "分支起点"));
    assert_eq!(app.session_id(), Some("sid-1"));
}

/// HELP 弹层（NORMAL `?`）：打开派生 Help 模式，Esc/`?` 关闭后
/// 回到 NORMAL（底层 mode 字段未动；层导航归 Esc 专属，`q` 不关闭）；
/// j/k 滚动、g/G 顶/底，上限由渲染回写钳制。
#[test]
fn help_overlay_opens_scrolls_and_closes() {
    let mut app = app();
    // INSERT 下 `?` 是普通字符（输入语义不被抢占）
    app.press(Key::Char('?'));
    assert_eq!(app.input.text(), "?");
    assert_eq!(app.mode(), Mode::Insert);

    app.press(Key::Esc);
    assert_eq!(app.mode(), Mode::Normal);
    // 打开：派生 Help 模式
    assert!(app.press(Key::Char('?')).is_empty());
    assert_eq!(app.mode(), Mode::Help);
    assert!(app.help_open());

    // 滚动：k 在顶部不动，j 下移（上限由渲染时 widget 钳制，见 ui 渲染测试）
    app.press(Key::Char('k'));
    assert_eq!(app.help_scroll, Some(0));
    app.press(Key::Char('j'));
    app.press(Key::Char('j'));
    assert_eq!(app.help_scroll, Some(2));
    // G 设为上界（渲染时钳到实际上限）、g 回顶
    app.press(Key::Char('G'));
    assert_eq!(app.help_scroll, Some(u16::MAX));
    app.press(Key::Char('g'));
    assert_eq!(app.help_scroll, Some(0));

    // 其余按键不污染输入缓冲（打开前的草稿 `?` 原样保留）
    assert!(app.press(Key::Char('x')).is_empty());
    assert_eq!(app.input.text(), "?");

    // Esc 关闭，回到 NORMAL
    assert!(app.press(Key::Esc).is_empty());
    assert_eq!(app.mode(), Mode::Normal);
    assert!(!app.help_open());

    // q 不关闭（层导航归 Esc 专属）；`?` 同样关闭
    app.press(Key::Char('?'));
    app.press(Key::Char('q'));
    assert_eq!(app.mode(), Mode::Help, "q 在 HELP 弹层不绑定");
    app.press(Key::Char('?'));
    assert_eq!(app.mode(), Mode::Normal);
}

/// NORMAL `g` 是完整动作（less 式到顶，无 pending 状态），随后 `?`
/// 正常打开帮助弹层。
#[test]
fn help_opens_after_scroll_to_top() {
    let mut app = app();
    app.press(Key::Esc);
    app.press(Key::Char('g'));
    assert_eq!(app.chat.scroll(), u16::MAX);
    assert!(app.press(Key::Char('?')).is_empty());
    assert_eq!(app.mode(), Mode::Help);
}
