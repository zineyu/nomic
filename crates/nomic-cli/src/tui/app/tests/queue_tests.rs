//! 队列 / 运行中 Enter / 模板展开相关测试。

use super::*;

#[test]
fn compaction_events_render_as_system_lines() {
    let mut app = app();
    app.handle_event(&AgentEvent::CompactionStart {
        tokens_before: 150_000,
    });
    // 压缩中只置状态栏提示，不进聊天区（失败时不残留）
    assert!(app.chat.items.is_empty());
    assert!(app.notice.as_deref().is_some_and(|n| n.contains("压缩")));
    app.handle_event(&AgentEvent::CompactionEnd {
        summary: "## Goal\nwork".to_string(),
        tokens_before: 150_000,
        kept_count: 7,
        usage: Usage::default(),
    });
    assert!(app.notice.is_none());
    let system_lines: Vec<&str> = app
        .chat
        .items
        .iter()
        .filter_map(|item| match item {
            ChatItem::System(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(system_lines.len(), 1, "{system_lines:?}");
    assert!(system_lines[0].contains("150000"), "{system_lines:?}");
    assert!(system_lines[0].contains('7'), "{system_lines:?}");
}

#[test]
fn summary_message_renders_compactly_in_history() {
    let mut app = app();
    app.load_history(&[
        nomic_ai::summary_message("## Goal\nearlier work", 1_000),
        Message::User(UserMessage {
            content: UserMessageContent::Text("recent question".to_string()),
            timestamp: 2_000,
        }),
    ]);
    assert!(matches!(&app.chat.items[0], ChatItem::System(text) if text.contains("已压缩")));
    assert!(matches!(&app.chat.items[1], ChatItem::User(text) if text == "recent question"));
}

#[test]
fn skill_completion_after_colon_prefix() {
    let mut app = app();
    app.command.set_available_skills(vec![
        SkillEntry {
            name: "jujutsu".to_string(),
            description: "jj vcs".to_string(),
            scope: SkillScope::Project,
        },
        SkillEntry {
            name: "rust-review".to_string(),
            description: "review rust".to_string(),
            scope: SkillScope::AgentUser,
        },
    ]);
    for c in "/skill:".chars() {
        app.command.insert_char(c);
    }
    let completion = app.command.completion().expect("/skill: 弹出全部 skill");
    assert_eq!(
        candidate_fragments(completion),
        vec!["skill:jujutsu", "skill:rust-review"]
    );

    // Tab 接受选中项；接受后候选收敛到精确匹配项，再次 Tab 保持不变
    app.command.tab_complete();
    assert_eq!(app.command.text(), "/skill:jujutsu");
    app.command.tab_complete();
    assert_eq!(app.command.text(), "/skill:jujutsu");

    // 前缀过滤后 Enter 填入唯一候选，再次 Enter 精确匹配放行提交
    app.command.take_input();
    for c in "/skill:juj".chars() {
        app.command.insert_char(c);
    }
    let completion = app.command.completion().expect("前缀过滤");
    assert_eq!(candidate_fragments(completion), vec!["skill:jujutsu"]);
    assert!(app.command.accept_completion_on_enter());
    assert_eq!(app.command.text(), "/skill:jujutsu");
    assert!(!app.command.accept_completion_on_enter());
}

#[test]
fn skill_load_message_renders_compactly_in_chat_and_history() {
    let skill = ActivatedSkill {
        name: "jujutsu".to_string(),
        scope: SkillScope::Project,
        path: PathBuf::from("/repo/.agents/skills/jujutsu/SKILL.md"),
        root: PathBuf::from("/repo/.agents/skills/jujutsu"),
        instructions: "do jj things".to_string(),
    };
    let message = skill_load_message(&skill, None);
    assert!(
        message.starts_with(
            "<active_skill name=\"jujutsu\" scope=\"project\" \
                 path=\"/repo/.agents/skills/jujutsu/SKILL.md\">"
        ),
        "{message}"
    );
    assert!(message.contains("do jj things"));
    assert!(message.contains("manually loaded"));
    assert!(!message.contains("\n\nUser: "));

    // 附带 args：注入消息尾部追加 User: <args>
    let message = skill_load_message(&skill, Some("只看 unsafe 块"));
    assert!(message.ends_with("\n\nUser: 只看 unsafe 块"));

    // 运行中注入：聊天区压缩为一行系统样式提示
    let mut injected = app();
    injected.handle_event(&AgentEvent::MessageStart(user_message(&message)));
    assert_eq!(injected.chat.items.len(), 1);
    let ChatItem::System(text) = &injected.chat.items[0] else {
        panic!("expected compact system item");
    };
    assert!(text.contains("jujutsu"), "{text}");
    assert!(text.contains("SKILL.md"), "{text}");

    // resume 恢复历史时同样压缩
    let mut resumed = app();
    resumed.load_history(&[Message::User(UserMessage {
        content: UserMessageContent::Text(message),
        timestamp: 0,
    })]);
    assert!(matches!(resumed.chat.items[0], ChatItem::System(_)));

    // 普通 user 消息不受影响
    let mut plain = app();
    plain.handle_event(&AgentEvent::MessageStart(user_message("普通问题")));
    assert!(matches!(plain.chat.items[0], ChatItem::User(_)));
}

#[test]
fn skill_list_text_lists_names_or_reports_empty() {
    assert!(skill_list_text(&[]).contains("没有可用的 skill"));
    let entry = SkillEntry {
        name: "jujutsu".to_string(),
        description: "jj vcs".to_string(),
        scope: SkillScope::Project,
    };
    let text = skill_list_text(&[entry]);
    assert!(text.contains("/skill:<name>"), "{text}");
    assert!(text.contains("jujutsu — jj vcs（project）"), "{text}");
}

#[test]
fn system_item_and_clear_items() {
    let mut app = app();
    app.chat.push_system(help_text());
    assert_eq!(app.chat.items.len(), 1);
    let ChatItem::System(text) = &app.chat.items[0] else {
        panic!("expected system item");
    };
    assert!(text.contains("/help"));
    assert!(text.contains("/new"));
    assert!(text.contains("/skill"));
    assert!(text.contains("/quit"));
    assert!(text.contains("/exit"));
    app.chat.clear_items();
    assert!(app.chat.items.is_empty());
}

#[test]
fn dismiss_completion_reports_whether_popup_was_open() {
    let mut app = app();
    assert!(!app.command.dismiss_completion());
    app.command.insert_char('/');
    assert!(app.command.dismiss_completion());
    assert!(app.command.completion().is_none());
    // 关闭后下次编辑会重新计算
    app.command.insert_char('n');
    assert!(app.command.completion().is_some());
}

#[test]
fn tick_advances_spinner_frame() {
    let mut app = app();
    let first = app.spinner();
    app.tick();
    assert_ne!(app.spinner(), first);
}

#[test]
fn scroll_is_saturating() {
    let mut app = app();
    app.chat.scroll_up(3);
    app.chat.scroll_up(5);
    assert_eq!(app.chat.scroll, 8);
    app.chat.scroll_down(10);
    assert_eq!(app.chat.scroll, 0);
    app.chat.scroll_up(u16::MAX);
    app.chat.scroll_up(1);
    assert_eq!(app.chat.scroll, u16::MAX);
}

#[test]
fn history_loads_as_items() {
    let messages = vec![
        *user_message("问题"),
        *assistant_message(
            vec![
                AssistantContent::Thinking(ThinkingContent {
                    thinking: "思考".to_string(),
                    thinking_signature: None,
                    redacted: false,
                }),
                text_block("回答"),
            ],
            StopReason::Stop,
            None,
        ),
    ];
    let mut app = app();
    app.load_history(&messages);
    assert_eq!(app.chat.items.len(), 2);
    let ChatItem::User(text) = &app.chat.items[0] else {
        panic!("expected user item");
    };
    assert_eq!(text, "问题");
    let ChatItem::Assistant(item) = &app.chat.items[1] else {
        panic!("expected assistant item");
    };
    assert!(item.done);
    assert_eq!(item.blocks.len(), 2);
}

// ── press 语义分发（新接口） ────────────────────────────────────────────

#[test]
fn enter_submits_prompt_with_attachments_and_marks_running() {
    let mut app = app();
    app.input.stage_image("a.png".to_string(), image());
    app.paste_text("描述这张图");
    let effects = app.press(Key::Enter);
    // running 在效果返回前已置位，避免提交空窗期重复提交
    assert!(app.is_running());
    let [Effect::Prompt { text, images }] = &effects[..] else {
        panic!("expected single Prompt effect");
    };
    assert_eq!(text, "描述这张图");
    assert_eq!(images.len(), 1);
    // 附件随提交带走，输入缓冲已清空
    assert!(!app.input.has_attachments());
    assert_eq!(app.input.text(), "");
}

#[test]
fn template_completion_lists_templates_with_commands() {
    let mut prefixed = app();
    prefixed.command.set_available_templates(vec![
        template("review", "Review $@", Some("<path>")),
        template("component", "Create $1", None),
    ]);
    for c in "/re".chars() {
        prefixed.command.insert_char(c);
    }
    let completion = prefixed.command.completion().expect("前缀弹出候选");
    assert_eq!(
        candidate_fragments(completion),
        vec!["resume", "retry", "review"]
    );

    // Tab 填入首个候选（接受后候选收敛到精确匹配，再次 Tab 不变）
    prefixed.command.tab_complete();
    assert_eq!(prefixed.command.text(), "/resume");
    prefixed.command.tab_complete();
    assert_eq!(prefixed.command.text(), "/resume");

    // 唯一前缀时 Tab 直接填入模板候选
    let mut unique = app();
    unique
        .command
        .set_available_templates(vec![template("review", "Review $@", Some("<path>"))]);
    for c in "/rev".chars() {
        unique.command.insert_char(c);
    }
    assert_eq!(
        candidate_fragments(unique.command.completion().expect("唯一候选")),
        vec!["review"]
    );
    unique.command.tab_complete();
    assert_eq!(unique.command.text(), "/review");

    // 空片段时模板与内建命令一起出现
    let mut empty = app();
    empty
        .command
        .set_available_templates(vec![template("zz-top", "body", None)]);
    empty.command.insert_char('/');
    let completion = empty.command.completion().expect("全部候选");
    assert!(candidate_fragments(completion).contains(&"zz-top".to_string()));
}

#[test]
fn enter_expands_template_invocation_into_prompt() {
    let mut spaced = app();
    spaced.command.set_available_templates(vec![template(
        "greet",
        "Hello $1, from ${2:-nomic}",
        None,
    )]);
    open_command(&mut spaced, "greet world \"a b\"");
    let effects = spaced.press(Key::Enter);
    assert!(spaced.is_running());
    assert_eq!(spaced.mode(), Mode::Normal, "命令受理后回 NORMAL");
    let [Effect::Prompt { text, images }] = &effects[..] else {
        panic!("expected single Prompt effect");
    };
    assert_eq!(text, "Hello world, from a b");
    assert!(images.is_empty());

    // 冒号形式同样展开
    let mut colon = app();
    colon
        .command
        .set_available_templates(vec![template("greet", "Hello $1", None)]);
    open_command(&mut colon, "greet:world");
    let [Effect::Prompt { text, .. }] = &colon.press(Key::Enter)[..] else {
        panic!("expected single Prompt effect");
    };
    assert_eq!(text, "Hello world");
}

#[test]
fn template_invocation_errors_and_builtin_precedence() {
    let mut quoted = app();
    quoted.command.set_available_templates(vec![
        template("greet", "Hello $1", None),
        // 与内建命令同名的模板不抢占 /help
        template("help", "template help", None),
    ]);
    // 引号未闭合：提示参数形式不对，不提交，留在命令行供修正
    open_command(&mut quoted, "greet \"unterminated");
    assert!(quoted.press(Key::Enter).is_empty());
    assert!(!quoted.is_running());
    assert_eq!(quoted.mode(), Mode::Command, "被拒绝时留在命令行");
    assert_eq!(quoted.notice.as_deref(), Some("参数形式不对：引号未闭合"));

    // 未知命令：维持原提示
    let mut missing = app();
    open_command(&mut missing, "missing arg");
    assert!(missing.press(Key::Enter).is_empty());
    assert!(
        missing
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("未知命令 /missing"))
    );

    // 内建命令优先于同名模板
    let mut builtin = app();
    builtin
        .command
        .set_available_templates(vec![template("help", "template help", None)]);
    open_command(&mut builtin, "help");
    assert!(builtin.press(Key::Enter).is_empty());
    assert!(!builtin.is_running());
    assert!(
        matches!(&builtin.chat.items.last(), Some(ChatItem::System(text)) if text.contains("可用命令"))
    );
}

/// 运行中（ADR-0014）：普通输入 Enter 排入统一消息队列（当前
/// 步骤完成后注入本轮运行），暂存附件随入队消息一起带走。
#[test]
fn enter_while_running_queues_prompt_with_attachments() {
    let mut app = app();
    app.handle_event(&AgentEvent::AgentStart);
    app.input.stage_image("a.png".to_string(), image());
    app.paste_text("hi");
    assert!(app.press(Key::Enter).is_empty());
    assert_eq!(app.input.text(), "");
    assert_eq!(app.queue.len(), 1);
    assert!(!app.input.has_attachments());
    assert!(app.notice().is_some_and(|n| n.contains("已排队")));

    // 再排一条，附件只随各自的消息走
    app.paste_text("second");
    assert!(app.press(Key::Enter).is_empty());
    assert_eq!(app.queue.len(), 2);

    // drain 按 FIFO 取出并置 running（与用户提交同一口径）
    let Some(Effect::Prompt { text, images }) = app.drain_queue() else {
        panic!("expected Prompt effect from drain");
    };
    assert_eq!(text, "hi");
    assert_eq!(images.len(), 1);
    assert!(app.is_running());

    app.finish_run(None);
    let Some(Effect::Prompt { text, images }) = app.drain_queue() else {
        panic!("expected Prompt effect from drain");
    };
    assert_eq!(text, "second");
    assert!(images.is_empty());
    assert_eq!(app.queue.len(), 0);
    assert!(app.drain_queue().is_none());
}

/// INSERT `Ctrl+G`：产生 OpenEditor 效果（外部编辑器编辑草稿），
/// 模式不变（TUI 挂起在事件循环层处理）。
#[test]
fn ctrl_g_emits_open_editor_effect() {
    let mut app = app();
    app.paste_text("草稿");
    let effects = app.press(Key::Ctrl('g'));
    assert!(
        matches!(effects.as_slice(), [Effect::OpenEditor]),
        "期望单个 OpenEditor 效果，实际 {effects:?}"
    );
    assert_eq!(app.mode(), Mode::Insert);
    assert_eq!(
        app.input.text(),
        "草稿",
        "外部编辑器持有草稿副本，状态层不动输入"
    );
}

/// 编辑器写回：整体替换输入缓冲，光标移到末尾，\r\n 归一、尾部空白去除。
#[test]
fn editor_result_replaces_input() {
    let mut app = app();
    app.paste_text("草稿");
    app.apply_editor_result("第一行\r\n第二行\n\n");
    assert_eq!(app.mode(), Mode::Insert);
    assert_eq!(app.input.text(), "第一行\n第二行");
    assert_eq!(app.input.cursor, app.input.text().len());
}

/// 编辑器写回空白内容：保留原草稿并提示（保存空文件是常见误操作）。
#[test]
fn editor_empty_result_keeps_draft() {
    let mut app = app();
    app.paste_text("未发草稿");
    app.apply_editor_result("  \n\n");
    assert_eq!(app.input.text(), "未发草稿");
    assert!(app.notice().is_some_and(|n| n.contains("为空")));
}

/// 运行中：命令行提交的模板调用展开后入队，不直接提交。
#[test]
fn enter_while_running_queues_expanded_template() {
    let mut app = app();
    app.command
        .set_available_templates(vec![template("greet", "Hello $1", None)]);
    app.handle_event(&AgentEvent::AgentStart);
    open_command(&mut app, "greet world");
    assert!(app.press(Key::Enter).is_empty());
    assert!(app.is_running(), "排队不改变运行态");
    assert_eq!(app.mode(), Mode::Normal, "受理后回 NORMAL");
    assert_eq!(app.queue.len(), 1);
    app.finish_run(None);
    let Some(Effect::Prompt { text, .. }) = app.drain_queue() else {
        panic!("expected Prompt effect from drain");
    };
    assert_eq!(text, "Hello world");
}

/// 空闲 + 空草稿 + 队列有暂停消息：Enter 直接发送下一条。
#[test]
fn idle_enter_with_empty_draft_drains_queue() {
    let mut app = app();
    app.handle_event(&AgentEvent::AgentStart);
    app.paste_text("queued");
    app.press(Key::Enter);
    app.finish_run(Some("已取消".to_string()));
    // 异常结束后队列保留（drain 由事件循环按结束方式裁决，这里手动模拟）
    assert_eq!(app.queue.len(), 1);
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::Prompt { text, .. }] if text == "queued"));
    assert!(app.is_running());
}

// ── QUEUE 模式（ADR-0012，oil.nvim 式队列编辑）─────────────────────────

/// NORMAL `m` 的进入守卫：队列为空或草稿非空时拒绝并提示。
#[test]
fn queue_mode_enter_guards() {
    let mut empty = app();
    empty.press(Key::Esc);
    empty.press(Key::Char('m'));
    assert!(!empty.queue_mode_active());
    assert!(empty.notice().is_some_and(|n| n.contains("队列为空")));

    let mut drafting = queued_app();
    drafting.paste_text("未发草稿");
    drafting.press(Key::Esc);
    drafting.press(Key::Char('m'));
    assert!(!drafting.queue_mode_active());
    assert!(drafting.notice().is_some_and(|n| n.contains("草稿非空")));

    // 草稿清空后可进入
    drafting.press(Key::Char('i'));
    drafting.press(Key::Ctrl('u'));
    drafting.press(Key::Esc);
    drafting.press(Key::Char('m'));
    assert!(drafting.queue_mode_active());
    assert_eq!(drafting.queue.cursor(), 0);
}

/// QUEUE 导航：j/k 钳制移动、G/g 跳队尾/队首、dd 删除游标条目，
/// 删空队列自动退出回 NORMAL。
#[test]
fn queue_mode_navigate_and_delete() {
    let mut app = queued_app();
    app.press(Key::Esc);
    app.press(Key::Char('m'));
    assert!(app.queue_mode_active());

    app.press(Key::Char('j'));
    assert_eq!(app.queue.cursor(), 1);
    app.press(Key::Char('j'));
    assert_eq!(app.queue.cursor(), 1, "到底钳制");
    app.press(Key::Char('g'));
    assert_eq!(app.queue.cursor(), 0);
    app.press(Key::Char('G'));
    assert_eq!(app.queue.cursor(), 1);

    // dd 删除队尾条目，游标收钳到新的末尾
    app.press(Key::Char('d'));
    app.press(Key::Char('d'));
    assert_eq!(app.queue.len(), 1);
    assert_eq!(app.queue.cursor(), 0);
    // 再删即空：退出 QUEUE 回 NORMAL 并提示
    app.press(Key::Char('x'));
    assert_eq!(app.queue.len(), 0);
    assert!(!app.queue_mode_active());
    assert_eq!(app.mode(), Mode::Normal);
    assert!(app.notice().is_some_and(|n| n.contains("队列已清空")));
}

/// QUEUE `J`/`K`：条目下移/上移一位（换位后游标跟随条目）。
#[test]
fn queue_mode_swap_reorders() {
    let mut app = queued_app();
    app.press(Key::Esc);
    app.press(Key::Char('m'));
    app.press(Key::Char('J'));
    assert_eq!(app.queue.cursor(), 1);
    app.press(Key::Char('J'));
    assert_eq!(app.queue.cursor(), 1, "到底不再移动");
    // 退出 QUEUE（空闲）：drain 恢复，换位后的队首立即提交
    let effects = app.press(Key::Esc);
    assert!(matches!(&effects[..], [Effect::Prompt { text, .. }] if text == "second\n两行"));
    assert!(app.is_running());
    // 换位不影响条目自身附件
    app.finish_run(None);
    let Some(Effect::Prompt { text, images }) = app.drain_queue() else {
        panic!("expected Prompt effect from drain");
    };
    assert_eq!(text, "first");
    assert_eq!(images.len(), 1);
}

/// QUEUE 就地编辑：`i` 载入槽位文本进草稿缓冲，Enter 保存写回；
/// 附件保留在槽位上。
#[test]
fn queue_mode_edit_and_save() {
    let mut app = queued_app();
    app.press(Key::Esc);
    app.press(Key::Char('m'));
    app.press(Key::Char('i'));
    assert!(app.queue.is_editing());
    assert_eq!(app.input.text(), "first");
    app.paste_text(" edited");
    app.press(Key::Enter);
    assert!(!app.queue.is_editing(), "保存后回到导航子状态");
    assert!(app.queue_mode_active());
    assert_eq!(app.input.text(), "");

    // 退出 QUEUE（空闲）：编辑后的队首提交，附件保留
    let effects = app.press(Key::Esc);
    assert!(
        matches!(&effects[..], [Effect::Prompt { text, images }] if text == "first edited" && images.len() == 1)
    );
}

/// QUEUE 编辑子状态：补全不启用（`/he` 不会弹补全），Enter 是保存
/// 而非接受候选或执行命令。
#[test]
fn queue_editing_disables_completion() {
    let mut app = queued_app();
    app.press(Key::Esc);
    app.press(Key::Char('m'));
    app.press(Key::Char('i'));
    app.press(Key::Ctrl('u'));
    app.paste_text("/he");
    assert!(app.input.completion().is_none());
    app.press(Key::Enter);
    let effects = app.press(Key::Esc);
    assert!(matches!(&effects[..], [Effect::Prompt { text, .. }] if text == "/he"));
}

/// QUEUE `o`：游标下方插入空槽位并就地编辑；保存空文本即撤销槽位。
#[test]
fn queue_mode_insert_slot_and_empty_save_discards() {
    let mut app = queued_app();
    app.press(Key::Esc);
    app.press(Key::Char('m'));
    app.press(Key::Char('o'));
    assert!(app.queue.is_editing());
    assert_eq!(app.queue.len(), 3);
    app.paste_text("inserted");
    app.press(Key::Esc); // Esc 同样保存
    assert_eq!(app.queue.len(), 3);
    assert!(!app.queue.is_editing());

    // 保存空文本：槽位被删除（oil.nvim 空行忽略语义）
    app.press(Key::Char('o'));
    app.press(Key::Esc);
    assert_eq!(app.queue.len(), 3);

    // 退出 QUEUE 恢复发送，顺序验证：first, inserted, second
    let mut texts = Vec::new();
    let mut effects = app.press(Key::Esc);
    while let Some(Effect::Prompt { text, .. }) = effects.pop() {
        texts.push(text);
        app.finish_run(None);
        effects = app.drain_queue().into_iter().collect();
    }
    assert_eq!(texts, ["first", "inserted", "second\n两行"]);
}
