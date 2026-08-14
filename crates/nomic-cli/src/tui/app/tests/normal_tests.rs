//! NORMAL 模式 / 会话快捷键 / 搜索相关测试。

use super::*;

/// QUEUE 打开期间 drain 冻结；退出时恢复：空闲即取出队首提交，
/// 运行中不产生效果（等本轮结束后自动 drain）。
#[test]
fn queue_mode_freezes_drain_and_leave_resumes() {
    // 运行中进入 QUEUE：drain 冻结，退出不产生效果
    let mut running = queued_app();
    running.handle_event(&AgentEvent::AgentStart);
    running.press(Key::Esc);
    running.press(Key::Char('m'));
    assert!(running.drain_queue().is_none(), "QUEUE 打开期间冻结 drain");
    assert!(running.press(Key::Esc).is_empty(), "运行中退出不提交");
    assert_eq!(running.mode(), Mode::Normal);
    // 退出后恢复：本轮正常结束后可 drain
    running.finish_run(None);
    assert!(matches!(running.drain_queue(), Some(Effect::Prompt { .. })));

    // 空闲退出 QUEUE：立即取出队首提交
    let mut idle = queued_app();
    idle.press(Key::Esc);
    idle.press(Key::Char('m'));
    let effects = idle.press(Key::Esc);
    assert!(matches!(&effects[..], [Effect::Prompt { text, .. }] if text == "first"));
    assert!(idle.is_running());
    assert_eq!(idle.queue.len(), 1);
}

/// 统一队列 QUEUE 模式（ADR-0014）：进入 QUEUE 冻结注入、退出
/// 解冻；导航/换位/就地编辑直接作用于队列；恢复发送按 FIFO。
#[test]
fn queue_mode_unified_queue_editing() {
    let mut app = app();
    app.handle_event(&AgentEvent::AgentStart);
    app.paste_text("msg-1");
    app.press(Key::Enter);
    app.paste_text("msg-2");
    app.press(Key::Enter);
    app.paste_text("msg-3");
    app.press(Key::Enter);
    // 异常结束（取消）：队列暂停保留，空闲下进入 QUEUE 编辑
    app.finish_run(Some("已取消".to_string()));
    app.press(Key::Esc);
    app.press(Key::Char('m'));
    assert!(app.queue_mode_active());
    assert_eq!(app.queue.len(), 3);
    // 进入 QUEUE 即冻结注入（core 在 turn 边界不再弹出）
    assert!(app.queue.handle().is_frozen());

    // 导航与换位：msg-1/msg-2 交换
    app.press(Key::Char('j'));
    app.press(Key::Char('j'));
    assert_eq!(app.queue.cursor(), 2);
    app.press(Key::Char('k'));
    app.press(Key::Char('k'));
    app.press(Key::Char('J'));
    assert_eq!(app.queue.cursor(), 1);

    // 就地编辑第三条
    app.press(Key::Char('G'));
    app.press(Key::Char('i'));
    assert_eq!(app.input.text(), "msg-3");
    app.paste_text("-edited");
    app.press(Key::Enter);

    // 退出 QUEUE：解冻；恢复发送按 FIFO（换位后 msg-2 在首）
    let effects = app.press(Key::Esc);
    assert!(!app.queue.handle().is_frozen());
    assert!(matches!(&effects[..], [Effect::Prompt { text, .. }] if text == "msg-2"));
    app.finish_run(None);
    let Some(Effect::Prompt { text, .. }) = app.drain_queue() else {
        panic!("expected Prompt effect from drain");
    };
    assert_eq!(text, "msg-1");
    app.finish_run(None);
    let Some(Effect::Prompt { text, .. }) = app.drain_queue() else {
        panic!("expected Prompt effect from drain");
    };
    assert_eq!(text, "msg-3-edited");
    assert_eq!(app.queue.len(), 0);
}

/// 运行中（含工具执行中）：本地 slash 命令照常执行，不被工具调用阻塞。
#[test]
fn enter_while_running_allows_local_slash_commands() {
    let mut app = app();
    app.handle_event(&AgentEvent::AgentStart);

    // /help 就地输出可用命令，不产生效果
    open_command(&mut app, "help");
    assert!(app.press(Key::Enter).is_empty());
    assert!(
        matches!(app.chat.items.last(), Some(ChatItem::System(text)) if text.contains("可用命令"))
    );
    assert_eq!(app.mode(), Mode::Normal, "命令受理后回 NORMAL");

    // /copy 返回 CopyText 效果（复制源为聊天区最新消息）
    app.chat
        .items
        .push(ChatItem::User("已发的消息".to_string()));
    open_command(&mut app, "copy");
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::CopyText(text)] if text == "已发的消息"));

    // /quit 运行中同样生效
    open_command(&mut app, "quit");
    assert!(app.press(Key::Enter).is_empty());
    assert!(app.should_quit());
}

/// 运行中：会话命令（经 driver 修改 agent 上下文）仍须等本轮结束，
/// 命令行输入保留（留在 COMMAND）供结束后提交。
#[test]
fn enter_while_running_blocks_session_commands() {
    let mut app = app();
    app.handle_event(&AgentEvent::AgentStart);
    for input in [
        "/new",
        "/resume",
        "/tree",
        "/compact",
        "/retry",
        "/models",
        "/skill:jujutsu",
    ] {
        open_command(&mut app, &input[1..]);
        assert!(
            app.press(Key::Enter).is_empty(),
            "{input} 运行中不应产生效果"
        );
        assert!(
            app.notice().is_some_and(|n| n.contains("运行中")),
            "{input} 应提示运行中"
        );
        assert_eq!(app.mode(), Mode::Command, "{input} 被拒绝时留在命令行");
        assert_eq!(app.command.text(), input, "{input} 输入应保留");
        app.leave_command();
    }
}

/// 补全弹层未精确匹配时 Enter 先填入候选，再次 Enter 执行命令
///（运行中的本地命令同一口径）。
#[test]
fn enter_while_running_accepts_completion_before_dispatch() {
    let mut app = app();
    app.handle_event(&AgentEvent::AgentStart);
    open_command(&mut app, "he");
    assert!(app.command.completion().is_some());
    // 第一次 Enter：填入补全候选，不提交
    assert!(app.press(Key::Enter).is_empty());
    assert_eq!(app.command.text(), "/help");
    assert_eq!(app.mode(), Mode::Command, "填入候选后留在命令行");
    // 第二次 Enter：精确匹配，执行本地命令后回 NORMAL
    assert!(app.press(Key::Enter).is_empty());
    assert_eq!(app.mode(), Mode::Normal);
    assert!(
        matches!(app.chat.items.last(), Some(ChatItem::System(text)) if text.contains("可用命令"))
    );
}

#[test]
fn slash_new_returns_effect_and_start_new_conversation_resets() {
    let mut app = app();
    app.chat.push_system("旧内容");
    open_command(&mut app, "new");
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::NewSession]));
    assert!(!app.is_running());
    // 事件循环执行效果：重置聊天区并切换 session
    app.start_new_conversation();
    app.set_session("new-id".to_string());
    assert_eq!(app.chat.items().len(), 1);
    assert!(matches!(&app.chat.items()[0], ChatItem::System(t) if t.contains("新对话")));
    assert_eq!(app.session_id(), Some("new-id"));
}

#[test]
fn compact_returns_effect_with_instructions_and_marks_running() {
    let mut app = app();
    open_command(&mut app, "compact 专注测试");
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::Compact(Some(i))] if i == "专注测试"));
    assert!(app.is_running());
}

#[test]
/// INSERT `Ctrl+C`：非空草稿先清草稿（不退出），草稿已空退出；
/// 运行中空草稿退出时先中断本轮。
fn ctrl_c_clears_draft_then_quits() {
    // 非空草稿：清草稿，不退出
    let mut drafting = app();
    drafting.paste_text("未发内容");
    assert!(drafting.press(Key::Ctrl('c')).is_empty());
    assert!(drafting.input.text().is_empty());
    assert!(!drafting.should_quit());

    // 草稿已空：退出
    let mut idle = app();
    assert!(idle.press(Key::Ctrl('c')).is_empty());
    assert!(idle.should_quit());

    // 运行中空草稿：中断并退出
    let mut running = app();
    running.handle_event(&AgentEvent::AgentStart);
    let effects = running.press(Key::Ctrl('c'));
    assert!(matches!(&effects[..], [Effect::Cancel]));
    assert!(running.should_quit());
}

/// Esc 逐层退回（ADR-0021）：INSERT→NORMAL（运行中亦然），NORMAL
/// 运行中 Esc 中断、空闲回 INSERT；COMMAND 先关补全再放弃。
#[test]
fn esc_retreat_stack() {
    // 运行中：INSERT Esc 进 NORMAL 浏览（不中断）；NORMAL Esc 才中断
    let mut running = app();
    running.handle_event(&AgentEvent::AgentStart);
    assert!(running.press(Key::Esc).is_empty());
    assert_eq!(running.mode(), Mode::Normal);
    assert!(running.is_running(), "INSERT Esc 不影响运行");
    assert!(matches!(&running.press(Key::Esc)[..], [Effect::Cancel]));
    assert!(running.is_running(), "中断效果由事件循环落实，状态层不置位");

    // 1. INSERT 空闲：进 NORMAL，无模式切换提示（草稿不受 Esc 影响）
    let mut app = app();
    app.paste_text("/h");
    assert!(app.press(Key::Esc).is_empty());
    assert_eq!(app.mode(), Mode::Normal);
    assert!(app.notice().is_none(), "进 NORMAL 不再提示");
    assert_eq!(app.input.text(), "/h", "草稿不受 Esc 影响");

    // 2. COMMAND：先关补全弹层（留在命令行），再放弃回 NORMAL（缓冲清空）
    assert!(app.press(Key::Char(':')).is_empty());
    assert_eq!(app.mode(), Mode::Command);
    assert!(app.command.completion().is_some());
    assert!(app.press(Key::Esc).is_empty());
    assert_eq!(app.mode(), Mode::Command, "关弹层后留在命令行");
    assert!(app.command.completion().is_none());
    assert_eq!(app.command.text(), "/", "命令文本不受 Esc 影响");
    assert!(app.press(Key::Esc).is_empty());
    assert_eq!(app.mode(), Mode::Normal);
    assert_eq!(app.command.text(), "", "命令缓冲随离开清空");
    assert_eq!(app.input.text(), "/h", "草稿与命令缓冲各自独立");

    // 3. 进 NORMAL 不覆盖既有提示
    app.press(Key::Char('i'));
    app.warn("其他提示");
    app.press(Key::Esc);
    assert_eq!(app.notice(), Some("其他提示"), "进 NORMAL 不覆盖既有提示");
}

/// NORMAL：j/k 滚动，字符不污染输入缓冲（草稿保留），
/// i 回原光标、Enter 到输入末尾返回 INSERT。
#[test]
fn normal_mode_navigates_and_preserves_draft() {
    let mut app = app();
    app.paste_text("草稿内容");
    let draft_len = app.input.text().len();
    app.press(Key::Esc);
    assert_eq!(app.mode(), Mode::Normal);

    // 字符不进入缓冲；j/k 滚动
    assert!(app.press(Key::Char('x')).is_empty());
    assert_eq!(app.input.text(), "草稿内容");
    app.press(Key::Char('k'));
    assert_eq!(app.chat.scroll(), 1);
    app.press(Key::Char('j'));
    assert_eq!(app.chat.scroll(), 0);

    // i 回 INSERT，草稿与光标位置保留
    assert!(app.press(Key::Char('i')).is_empty());
    assert_eq!(app.mode(), Mode::Insert);
    assert_eq!(app.input.text(), "草稿内容");

    // Enter 回 INSERT：光标到输入末尾（「草稿内容」4 个 CJK 字符，宽 8 列）
    app.press(Key::Home);
    app.press(Key::Esc);
    app.press(Key::Enter);
    assert_eq!(app.mode(), Mode::Insert);
    let (row, col) = app.input.cursor_position();
    assert_eq!((row, col), (0, 8), "光标在末尾：{row},{col}");
    assert_eq!(app.input.text().len(), draft_len);
}

/// NORMAL：g 到顶、G 回底（跟随模式）、d/u 半页滚动（less 式单键）。
#[test]
fn normal_mode_g_half_page_and_scroll() {
    let mut app = app();
    app.press(Key::Esc);

    app.press(Key::Char('g'));
    assert_eq!(
        app.chat.scroll(),
        u16::MAX,
        "g 滚到顶（几何同步时钳到上限）"
    );

    app.press(Key::Char('G'));
    assert_eq!(app.chat.scroll(), 0, "G 回底");

    app.press(Key::Char('u'));
    assert_eq!(app.chat.scroll(), 5);
    app.press(Key::Char('d'));
    assert_eq!(app.chat.scroll(), 0);

    // j/k 单行滚动
    app.press(Key::Char('k'));
    assert_eq!(app.chat.scroll(), 1);
    app.press(Key::Char('j'));
    assert_eq!(app.chat.scroll(), 0, "j 向下滚动钳在 0");
}

/// 几何在状态层主动计算：g 到顶的 u16::MAX 偏移经 sync_chat_geometry
/// 按视口钳到上限——滚动正确性不依赖先渲一帧。
#[test]
fn geometry_sync_clamps_scroll_without_render() {
    let mut app = app_with_history();
    app.press(Key::Esc);
    app.press(Key::Char('g'));
    app.sync_chat_geometry(40, 5);
    assert!(app.chat.scroll_max() > 0);
    assert_eq!(app.chat.scroll(), app.chat.scroll_max());
}

/// NORMAL：Y 复制最新一条消息（与 /copy 同效果）；无消息时提示。
#[test]
fn normal_mode_y_copies_latest_message() {
    let mut empty = app();
    empty.press(Key::Esc);
    assert!(empty.press(Key::Char('Y')).is_empty());
    assert_eq!(empty.notice(), Some("没有可复制的消息"));

    let mut app = app();
    app.load_history(&[*user_message("你好")]);
    app.press(Key::Esc);
    let effects = app.press(Key::Char('Y'));
    assert!(matches!(&effects[..], [Effect::CopyText(text)] if text == "你好"));
}

/// NORMAL：Ctrl+C 与 INSERT 同口径（运行中取消并退出，空闲退出）；
/// d/u 半页滚动。
#[test]
fn normal_mode_ctrl_c_quits_and_d_scrolls() {
    let mut idle = app();
    idle.press(Key::Esc);
    assert!(idle.press(Key::Ctrl('c')).is_empty());
    assert!(idle.should_quit());

    let mut running = app();
    running.press(Key::Esc);
    running.handle_event(&AgentEvent::AgentStart);
    assert!(matches!(
        &running.press(Key::Ctrl('c'))[..],
        [Effect::Cancel]
    ));
    assert!(running.should_quit(), "运行中 Ctrl+C 也退出");
}

/// NORMAL 会话快捷键（ADR-0021 修订）：`s` 恢复会话、`b` 会话树创建分支、
/// `c` 新建会话，均直达对应 Effect；运行中拒绝并提示。
#[test]
fn normal_session_shortcuts_dispatch_directly() {
    let mut app = app();
    app.press(Key::Esc);

    // s：恢复会话（列出 session 打开选择器）
    let effects = app.press(Key::Char('s'));
    assert!(matches!(&effects[..], [Effect::ListSessions]));
    // b：会话树（创建分支）
    let effects = app.press(Key::Char('b'));
    assert!(matches!(&effects[..], [Effect::ListTree]));
    // c：新建会话
    let effects = app.press(Key::Char('c'));
    assert!(matches!(&effects[..], [Effect::NewSession]));
    // 不再打开会话菜单 overlay
    assert!(app.picker().is_none());
    assert_eq!(app.mode(), Mode::Normal);

    // 运行中拒绝（与 COMMAND 下会话命令同一口径）
    app.handle_event(&AgentEvent::AgentStart);
    for key in ['s', 'b', 'c'] {
        assert!(
            app.press(Key::Char(key)).is_empty(),
            "运行中 `{key}` 不应产生效果"
        );
        assert!(
            app.notice().is_some_and(|n| n.contains("运行中")),
            "运行中 `{key}` 应提示"
        );
    }
}

/// INSERT 历史召回：↑ 上一条 / ↓ 下一条，到最新后还原暂存草稿；
/// 提交记录历史（去重相邻重复，新条目在前）。
#[test]
fn insert_history_recall() {
    let mut app = app();
    // 提交两条 prompt 进入历史
    app.paste_text("第一条");
    app.press(Key::Enter);
    app.finish_run(None);
    app.paste_text("第二条");
    app.press(Key::Enter);
    app.finish_run(None);
    assert_eq!(app.history, ["第二条", "第一条"]);

    // 输入未发草稿后 ↑：暂存草稿并召回最近一条
    app.paste_text("未发草稿");
    app.press(Key::Up);
    assert_eq!(app.input.text(), "第二条");
    // 再 ↑：更早一条
    app.press(Key::Up);
    assert_eq!(app.input.text(), "第一条");
    // ↓：回最近，再 ↓：还原暂存草稿并退出召回
    app.press(Key::Down);
    assert_eq!(app.input.text(), "第二条");
    app.press(Key::Down);
    assert_eq!(app.input.text(), "未发草稿");
    assert!(app.history_index.is_none());
}

/// INSERT `Ctrl+D`：空草稿退出，非空删除光标处字符（readline 语义）。
#[test]
fn ctrl_d_quits_on_empty_or_deletes_char() {
    let mut empty = app();
    assert!(empty.press(Key::Ctrl('d')).is_empty());
    assert!(empty.should_quit());

    let mut app = app();
    app.paste_text("abc");
    app.press(Key::Home);
    app.press(Key::Ctrl('d'));
    assert_eq!(app.input.text(), "bc");
    assert!(!app.should_quit());
}

/// picker 打开时模式派生为 Picker（ADR-0011）。
#[test]
fn mode_derives_picker_when_open() {
    let mut app = app();
    assert_eq!(app.mode(), Mode::Insert);
    app.open_resume_picker(vec![PickerRow {
        selectable: true,
        id: "s1".to_string(),
        text: "row".to_string(),
    }]);
    assert_eq!(app.mode(), Mode::Picker);
}

/// INSERT 词级编辑：Ctrl+W 删词、Ctrl+U 清到行首、Ctrl+A/E 行首/行尾、
/// Alt+B/F 词级移动；多行输入只作用当前逻辑行。
#[test]
fn insert_word_level_editing() {
    let cursor_col = |app: &App| app.input.cursor_position().1;

    // Ctrl+W：删前一个词连同词前空白
    {
        let mut app = app();
        app.paste_text("hello world  foo");
        app.press(Key::Ctrl('w'));
        assert_eq!(app.input.text(), "hello world  ");
        app.press(Key::Ctrl('w'));
        assert_eq!(app.input.text(), "hello ", "连空白间隔一起删");
    }

    // Alt+B/F：词级移动
    {
        let mut app = app();
        app.paste_text("foo bar baz");
        app.press(Key::Alt('b'));
        assert_eq!(cursor_col(&app), 8, "Alt+B 到所在词/前一词开头");
        app.press(Key::Alt('b'));
        assert_eq!(cursor_col(&app), 4);
        app.press(Key::Alt('b'));
        assert_eq!(cursor_col(&app), 0);
        app.press(Key::Alt('f'));
        assert_eq!(cursor_col(&app), 4, "Alt+F 到后一词开头");
        app.press(Key::Alt('f'));
        assert_eq!(cursor_col(&app), 8);
    }

    // Ctrl+U / Ctrl+A / Ctrl+E：多行只作用当前逻辑行
    let mut app = app();
    app.paste_text("first line\nsecond line");
    app.press(Key::Ctrl('a'));
    assert_eq!(app.input.cursor_position(), (1, 0), "Ctrl+A 到当前行首");
    app.press(Key::Ctrl('e'));
    assert_eq!(app.input.cursor_position(), (1, 11), "Ctrl+E 到当前行尾");
    app.press(Key::Ctrl('u'));
    assert_eq!(app.input.text(), "first line\n", "Ctrl+U 只清当前行");
    assert_eq!(app.input.cursor_position(), (1, 0));
}

/// 粘贴的意图是编辑：NORMAL 下粘贴先回到 INSERT（草稿保留）。
#[test]
fn paste_in_normal_returns_to_insert() {
    let mut app = app();
    app.paste_text("草稿");
    app.press(Key::Esc);
    assert_eq!(app.mode(), Mode::Normal);
    app.paste_text("追加");
    assert_eq!(app.mode(), Mode::Insert);
    assert_eq!(app.input.text(), "草稿追加");
}

#[test]
fn resume_picker_enter_returns_resume_effect() {
    let mut app = app();
    app.open_resume_picker(vec![
        PickerRow {
            selectable: true,
            id: "s1".to_string(),
            text: "row 1".to_string(),
        },
        PickerRow {
            selectable: true,
            id: "s2".to_string(),
            text: "row 2".to_string(),
        },
    ]);
    // picker 接管键位：↓ 移动选中项，普通字符进入过滤而非输入缓冲
    assert!(app.press(Key::Down).is_empty());
    assert_eq!(app.input.text(), "");
    let effects = app.press(Key::Enter);
    assert!(matches!(&effects[..], [Effect::Resume(id)] if id == "s2"));
    assert!(app.picker().is_none());
    // Esc 取消不产出效果
    app.open_resume_picker(vec![PickerRow {
        selectable: true,
        id: "s1".to_string(),
        text: "row 1".to_string(),
    }]);
    assert!(app.press(Key::Esc).is_empty());
    assert!(app.picker().is_none());
}

/// NORMAL `:`：进入专门的命令输入框（COMMAND 模式，ADR-0020）——
/// 独立缓冲预填 `/`（补全弹层列出全部命令），草稿保留不受影响。
#[test]
fn normal_colon_opens_command_input() {
    let mut app = app();
    app.paste_text("未发送的草稿");
    app.press(Key::Esc);
    assert!(app.press(Key::Char(':')).is_empty());
    assert_eq!(app.mode(), Mode::Command);
    assert_eq!(app.command.text(), "/");
    assert!(app.command.completion().is_some(), "命令补全弹层自动出现");
    assert_eq!(app.input.text(), "未发送的草稿", "草稿不受影响");

    // 空命令行（仅预填的 `/`）直接 Enter：无声返回 NORMAL，草稿仍在
    assert!(app.press(Key::Enter).is_empty());
    assert_eq!(app.mode(), Mode::Normal);
    assert_eq!(app.input.text(), "未发送的草稿");
}

/// ADR-0020：聊天输入框不再触发命令——`/` 开头的草稿按普通 prompt
/// 发送；运行中同样排队而非执行命令。
#[test]
fn insert_no_longer_triggers_slash_commands() {
    let mut app = app();
    app.paste_text("/help");
    let effects = app.press(Key::Enter);
    let [Effect::Prompt { text, images }] = &effects[..] else {
        panic!("expected single Prompt effect");
    };
    assert_eq!(text, "/help", "`/` 开头按普通 prompt 发送");
    assert!(images.is_empty());
    assert!(app.is_running());

    // 运行中：`/` 开头的输入排队（统一消息队列），不执行命令
    app.paste_text("/copy");
    assert!(app.press(Key::Enter).is_empty());
    assert_eq!(app.queue.len(), 1);
    assert!(!app.should_quit());
}

/// NORMAL 消息游标：进入时定位最新一条消息；[/] 在消息间移动（跳过
/// 工具与系统条目），{/} 在工具条目间移动；越界钳制。
#[test]
fn normal_cursor_steps_between_messages_and_tools() {
    let mut app = app_with_history();
    app.press(Key::Esc);
    // 条目布局：0 user, 1 assistant, 2 tool, 3 user, 4 assistant
    assert_eq!(
        app.chat.cursor_item,
        Some(4),
        "进入 NORMAL 定位最新一条消息"
    );

    // [ 逐条向前：assistant → user（跳过 tool）
    app.press(Key::Char('['));
    assert_eq!(app.chat.cursor_item, Some(3));
    app.press(Key::Char('['));
    assert_eq!(app.chat.cursor_item, Some(1), "跳过 tool 条目");
    // ] 回到尾部
    app.press(Key::Char(']'));
    assert_eq!(app.chat.cursor_item, Some(3));

    // { 定位工具条目；继续 { 越界钳制在原位
    app.press(Key::Char('{'));
    assert_eq!(app.chat.cursor_item, Some(2));
    app.press(Key::Char('{'));
    assert_eq!(app.chat.cursor_item, Some(2), "没有更早的工具条目，钳制");

    // g/G：游标随滚动到首/尾消息
    app.press(Key::Char('g'));
    assert_eq!(app.chat.cursor_item, Some(0));
    app.press(Key::Char('G'));
    assert_eq!(app.chat.cursor_item, Some(4));
}
