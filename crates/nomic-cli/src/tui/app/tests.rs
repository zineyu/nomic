#![allow(clippy::literal_string_with_formatting_args)]
// 测试数据包含模板占位符字面量（${2:-nomic} 等），并非格式化参数

use std::path::PathBuf;

use nomic_ai::{ApiKind, AssistantMessage, TextContent, ThinkingContent, Usage, UserMessage};
use nomic_core::{ToolResult, ToolUpdate};
use nomic_skills::SkillScope;

use nomic_ai::{AssistantContent, AssistantEvent, UserContent, UserMessageContent};
use nomic_prompts::PromptTemplate;
use nomic_skills::ActivatedSkill;

use super::chat::{AssistantItem, result_summary, user_text};
use super::input::skill_list_text;
use super::*;

fn user_message(text: &str) -> Box<Message> {
    Box::new(Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp: 0,
    }))
}

fn assistant_message(
    content: Vec<AssistantContent>,
    stop_reason: StopReason,
    error_message: Option<String>,
) -> Box<Message> {
    Box::new(Message::Assistant(AssistantMessage {
        content,
        api: ApiKind::AnthropicMessages,
        provider: "anthropic".to_string(),
        model: "claude".to_string(),
        response_model: None,
        response_id: None,
        usage: Usage::default(),
        stop_reason,
        error_message,
        timestamp: 0,
    }))
}

fn text_block(text: &str) -> AssistantContent {
    AssistantContent::Text(TextContent {
        text: text.to_string(),
        text_signature: None,
    })
}

fn app() -> App {
    App::new("test-model".to_string(), None, 200_000)
}

/// 打开浮层命令栏并键入命令文本（ADR-0020）：INSERT 下先 Esc 进
/// NORMAL，`:` 打开命令栏（空缓冲、无 `/` 前缀），粘贴命令文本。
fn open_command(app: &mut App, text: &str) {
    if app.mode() == Mode::Insert {
        app.press(Key::Esc);
    }
    app.press(Key::Char(':'));
    app.paste_text(text);
}

fn candidate_fragments(completion: &Completion) -> Vec<String> {
    completion
        .candidates
        .iter()
        .map(CompletionCandidate::fragment)
        .collect()
}

#[test]
fn accumulates_streaming_text_and_thinking() {
    let mut app = app();
    app.handle_event(&AgentEvent::MessageStart(assistant_message(
        Vec::new(),
        StopReason::Stop,
        None,
    )));
    app.handle_event(&AgentEvent::MessageUpdate(AssistantEvent::ThinkingStart {
        index: 0,
    }));
    app.handle_event(&AgentEvent::MessageUpdate(AssistantEvent::ThinkingDelta {
        index: 0,
        delta: "想一".to_string(),
    }));
    app.handle_event(&AgentEvent::MessageUpdate(AssistantEvent::ThinkingDelta {
        index: 0,
        delta: "想".to_string(),
    }));
    app.handle_event(&AgentEvent::MessageUpdate(AssistantEvent::TextStart {
        index: 1,
    }));
    app.handle_event(&AgentEvent::MessageUpdate(AssistantEvent::TextDelta {
        index: 1,
        delta: "你好".to_string(),
    }));
    app.handle_event(&AgentEvent::MessageEnd {
        message: assistant_message(Vec::new(), StopReason::Stop, None),
        context_tokens: 0,
    });

    let Some(ChatItem::Assistant(item)) = app.chat.items.first() else {
        panic!("expected assistant item");
    };
    assert!(item.done);
    assert!(item.error.is_none());
    assert_eq!(item.blocks.len(), 2);
    let [Block::Thinking(thinking), Block::Text(text)] = &item.blocks[..] else {
        panic!("unexpected blocks: {:?}", item.blocks);
    };
    assert_eq!(thinking, "想一想");
    assert_eq!(text, "你好");
}

#[test]
fn records_assistant_error_on_message_end() {
    let mut app = app();
    app.handle_event(&AgentEvent::MessageStart(assistant_message(
        Vec::new(),
        StopReason::Stop,
        None,
    )));
    app.handle_event(&AgentEvent::MessageEnd {
        message: assistant_message(
            Vec::new(),
            StopReason::Error,
            Some("rate limited".to_string()),
        ),
        context_tokens: 0,
    });
    let Some(ChatItem::Assistant(item)) = app.chat.items.first() else {
        panic!("expected assistant item");
    };
    assert_eq!(item.error.as_deref(), Some("rate limited"));
}

/// MessageEnd / AgentEnd 携带 core 的权威上下文估算，App 只抄不算
///（含错误响应：锚点规则只在 core 定义，状态层不再自行判别）。
#[test]
fn message_end_and_agent_end_copy_context_tokens() {
    let mut app = app();
    assert_eq!(app.context_tokens(), 0);

    app.handle_event(&AgentEvent::MessageEnd {
        message: assistant_message(Vec::new(), StopReason::Stop, None),
        context_tokens: 12_345,
    });
    assert_eq!(app.context_tokens(), 12_345);

    app.handle_event(&AgentEvent::MessageEnd {
        message: assistant_message(Vec::new(), StopReason::Error, Some("boom".to_string())),
        context_tokens: 12_400,
    });
    assert_eq!(app.context_tokens(), 12_400);

    app.handle_event(&AgentEvent::AgentEnd {
        messages: Vec::new(),
        context_tokens: 56_000,
    });
    assert_eq!(app.context_tokens(), 56_000);
}

/// 恢复历史（启动 / `resume`）时按估算口径初始化上下文用量。
#[test]
fn load_history_estimates_context_tokens() {
    let mut app = app();
    let text = "a".repeat(400);
    app.load_history(&[*user_message(&text)]);
    assert_eq!(app.context_tokens(), 100);
}

/// `new` 命令开启新对话时上下文用量清零。
#[test]
fn new_conversation_resets_context_tokens() {
    let mut app = app();
    app.set_context_tokens(12_345);
    app.start_new_conversation();
    assert_eq!(app.context_tokens(), 0);
}

#[test]
fn tracks_tool_execution_lifecycle() {
    let mut app = app();
    let args = serde_json::json!({"command": "ls"});
    app.handle_event(&AgentEvent::ToolExecutionStart {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        args,
    });
    app.handle_event(&AgentEvent::ToolExecutionUpdate {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        partial: ToolUpdate {
            content: vec![UserContent::Text(TextContent {
                text: "a\nb".to_string(),
                text_signature: None,
            })],
            details: None,
        },
    });
    app.handle_event(&AgentEvent::ToolExecutionEnd {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        result: ToolResult::text("done"),
        is_error: false,
    });

    let Some(ChatItem::Tool(tool)) = app.chat.items.first() else {
        panic!("expected tool item");
    };
    assert_eq!(tool.status, ToolStatus::Ok);
    assert_eq!(tool.detail, ["done"]);
    assert_eq!(tool.args, "ls");
}

#[test]
fn result_summary_keeps_last_lines() {
    let blocks = vec![UserContent::Text(TextContent {
        text: "l1\n\n  l2  \nl3\nl4\nl5\n\n".to_string(),
        text_signature: None,
    })];
    assert_eq!(result_summary(&blocks), ["l3", "l4", "l5"]);

    let empty = vec![UserContent::Text(TextContent {
        text: "\n  \n".to_string(),
        text_signature: None,
    })];
    assert!(result_summary(&empty).is_empty());
}

#[test]
fn matches_parallel_tools_by_id() {
    let mut app = app();
    for id in ["t1", "t2"] {
        app.handle_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: id.to_string(),
            tool_name: "read".to_string(),
            args: serde_json::json!({}),
        });
    }
    app.handle_event(&AgentEvent::ToolExecutionEnd {
        tool_call_id: "t1".to_string(),
        tool_name: "read".to_string(),
        result: ToolResult::text("ok"),
        is_error: true,
    });

    let [ChatItem::Tool(first), ChatItem::Tool(second)] = &app.chat.items[..] else {
        panic!("unexpected items");
    };
    assert_eq!(first.status, ToolStatus::Failed);
    assert_eq!(second.status, ToolStatus::Running);
}

#[test]
fn multiline_input_tracks_lines_and_cursor() {
    let mut app = app();
    assert_eq!(app.input.line_count(), 1);
    assert_eq!(app.input.cursor_position(), (0, 0));

    for c in "你好".chars() {
        app.input.insert_char(c);
    }
    app.input.insert_newline();
    for c in "ab".chars() {
        app.input.insert_char(c);
    }
    assert_eq!(app.input.text(), "你好\nab");
    assert_eq!(app.input.line_count(), 2);
    // 光标在第二行末尾：行号 1，行内宽度 2
    assert_eq!(app.input.cursor_position(), (1, 2));

    // 光标移回第一行行尾（CJK 宽度 4）
    app.input.cursor_left();
    app.input.cursor_left();
    app.input.cursor_left();
    assert_eq!(app.input.cursor_position(), (0, 4));

    // 多行输入可整体提交
    assert_eq!(app.input.take_input().as_deref(), Some("你好\nab"));
    assert_eq!(app.input.line_count(), 1);
}

#[test]
fn newline_dismisses_completion() {
    let mut app = app();
    app.command.insert_char('n');
    assert!(app.command.completion().is_some());
    // 换行是空白字符，命令补全随之关闭
    app.command.insert_newline();
    assert!(app.command.completion().is_none());
}

#[test]
fn input_editing_respects_char_boundaries() {
    let mut app = app();
    app.input.insert_char('你');
    app.input.insert_char('好');
    app.input.cursor_left();
    app.input.insert_char('a');
    assert_eq!(app.input.text(), "你a好");
    app.input.backspace();
    assert_eq!(app.input.text(), "你好");
    app.input.backspace();
    assert_eq!(app.input.text(), "好");
    assert_eq!(app.input.take_input().as_deref(), Some("好"));
    assert!(app.input.take_input().is_none());
}

#[test]
fn command_completion_filters_by_prefix_and_tab_cycles() {
    let mut app = app();
    // 空片段即全量候选（进入命令栏即列出全部命令）
    app.command.refresh_completion();
    let completion = app.command.completion().expect("空片段即全部候选");
    assert_eq!(completion.candidates.len(), COMMANDS.len());

    app.command.insert_char('n');
    let completion = app.command.completion().expect("n 匹配 new");
    assert_eq!(candidate_fragments(completion), vec!["new"]);

    // Tab 接受候选
    app.command.tab_complete();
    assert_eq!(app.command.text(), "new");
    // 精确匹配后仍显示（展示描述），且选中该项
    let completion = app.command.completion().expect("精确匹配仍显示候选");
    assert_eq!(completion.candidates[completion.selected].fragment(), "new");

    // 输入空格（进入参数区）后弹层消失
    app.command.insert_char(' ');
    assert!(app.command.completion().is_none());
}

#[test]
fn command_completion_matches_alias_and_enter_accepts() {
    let mut app = app();
    for c in "ex".chars() {
        app.command.insert_char(c);
    }
    let completion = app.command.completion().expect("ex 匹配别名 exit");
    assert_eq!(
        completion.candidates[completion.selected].fragment(),
        "quit"
    );

    // 未精确匹配时 Enter 先填入候选，不提交
    assert!(app.command.accept_completion_on_enter());
    assert_eq!(app.command.text(), "quit");
    // 精确匹配后 Enter 放行提交
    assert!(!app.command.accept_completion_on_enter());
}

#[test]
fn picker_clamps_selection_and_take_closes() {
    let mut app = app();
    let rows = (0..3)
        .map(|i| PickerRow {
            selectable: true,
            id: format!("id-{i}"),
            text: format!("row {i}"),
        })
        .collect();
    app.open_resume_picker(rows);

    // 到底/顶钳制，不循环
    app.picker.as_mut().expect("picker").select(1);
    app.picker.as_mut().expect("picker").select(1);
    app.picker.as_mut().expect("picker").select(1);
    assert_eq!(app.picker().expect("picker").core.selected, 2);
    app.picker.as_mut().expect("picker").select(-5);
    assert_eq!(app.picker().expect("picker").core.selected, 0);

    // Enter 确认：返回选中 id 并关闭；关闭后再次确认为 None
    app.picker.as_mut().expect("picker").select(1);
    assert_eq!(
        app.take_picker_selection(),
        Some((PickerKind::Resume, "id-1".to_string()))
    );
    assert!(app.picker().is_none());
    assert!(app.take_picker_selection().is_none());
}

#[test]
fn parse_command_dispatches_known_unknown_and_slash_prefixed() {
    // 仍以 `/` 开头：命令语法已无前缀（ADR-0020 修订），拒绝并提示
    assert_eq!(parse_command("/help"), CommandParse::SlashPrefixed);
    assert_eq!(parse_command("/"), CommandParse::SlashPrefixed);
    assert_eq!(
        parse_command("help"),
        CommandParse::Known(CommandAction::Help)
    );
    assert_eq!(
        parse_command("new"),
        CommandParse::Known(CommandAction::New)
    );
    assert_eq!(
        parse_command("resume"),
        CommandParse::Known(CommandAction::Resume)
    );
    assert_eq!(
        parse_command("quit"),
        CommandParse::Known(CommandAction::Quit)
    );
    assert_eq!(
        parse_command("exit"),
        CommandParse::Known(CommandAction::Quit)
    );
    assert_eq!(
        parse_command("copy"),
        CommandParse::Known(CommandAction::Copy)
    );
    assert_eq!(
        parse_command("thinking"),
        CommandParse::Known(CommandAction::Thinking)
    );
    assert_eq!(
        parse_command("goal"),
        CommandParse::Known(CommandAction::Goal)
    );
    assert_eq!(
        parse_command("continue"),
        CommandParse::Known(CommandAction::Continue)
    );
    assert_eq!(
        parse_command("foobar"),
        CommandParse::Unknown("foobar".to_string())
    );
    // 普通文本同样是未知命令（命令栏只承载命令；模板调用由分发层展开）
    assert_eq!(
        parse_command("hello"),
        CommandParse::Unknown("hello".to_string())
    );
    // 首尾空白容错
    assert_eq!(
        parse_command("  new  "),
        CommandParse::Known(CommandAction::New)
    );
}

#[test]
fn copy_takes_latest_message_text() {
    let mut app = app();
    // 空聊天区：无可复制内容，就地提示
    assert!(app.execute_command(CommandAction::Copy).is_empty());
    assert_eq!(app.notice.as_deref(), Some("没有可复制的消息"));

    app.chat
        .items
        .push(ChatItem::User("第一条问题".to_string()));
    app.chat.items.push(ChatItem::Assistant(AssistantItem {
        blocks: vec![
            Block::Thinking("内部推理".to_string()),
            Block::Text("第一段正文".to_string()),
            Block::Text("第二段正文".to_string()),
        ],
        done: true,
        error: None,
    }));
    // thinking 不复制，多个正文块以空行连接
    let [Effect::CopyText(text)] = &app.execute_command(CommandAction::Copy)[..] else {
        panic!("expected CopyText effect");
    };
    assert_eq!(text, "第一段正文\n\n第二段正文");

    // 最新一条是只有工具调用的 assistant 消息：向前找有正文的消息
    app.chat
        .items
        .push(ChatItem::Assistant(AssistantItem::default()));
    app.chat.items.push(ChatItem::User("最新问题".to_string()));
    let [Effect::CopyText(text)] = &app.execute_command(CommandAction::Copy)[..] else {
        panic!("expected CopyText effect");
    };
    assert_eq!(text, "最新问题");
}

#[test]
fn thinking_toggles_collapse_state() {
    let mut app = app();
    // 默认折叠，本地命令不产生外部效果
    assert!(app.thinking_collapsed());
    assert!(app.execute_command(CommandAction::Thinking).is_empty());
    assert!(!app.thinking_collapsed());
    assert!(app.execute_command(CommandAction::Thinking).is_empty());
    assert!(app.thinking_collapsed());
    // 每次切换在聊天区留下系统提示
    let systems = app
        .chat
        .items
        .iter()
        .filter(|item| matches!(item, ChatItem::System(_)))
        .count();
    assert_eq!(systems, 2);
    // 本地命令：运行中也可执行
    assert!(CommandAction::Thinking.is_local());
}

#[test]
fn goal_toggles_mode_state() {
    let mut app = app();
    // 默认关闭，本地命令不产生外部效果
    assert!(!app.goal_mode());
    assert!(app.execute_command(CommandAction::Goal).is_empty());
    assert!(app.goal_mode());
    assert!(app.execute_command(CommandAction::Goal).is_empty());
    assert!(!app.goal_mode());
    // 每次切换在聊天区留下系统提示
    let systems = app
        .chat
        .items
        .iter()
        .filter(|item| matches!(item, ChatItem::System(_)))
        .count();
    assert_eq!(systems, 2);
    // 本地命令：运行中也可执行
    assert!(CommandAction::Goal.is_local());
}

#[test]
fn continue_pops_trailing_failed_assistant_and_requests_continue() {
    let mut app = app();
    app.handle_event(&AgentEvent::MessageStart(user_message("hi")));
    app.handle_event(&AgentEvent::MessageStart(assistant_message(
        Vec::new(),
        StopReason::Error,
        Some("boom".to_string()),
    )));
    app.handle_event(&AgentEvent::MessageEnd {
        message: assistant_message(Vec::new(), StopReason::Error, Some("boom".to_string())),
        context_tokens: 0,
    });

    let effects = app.execute_command(CommandAction::Continue);

    // 失败条目随历史中的失败消息一并移除；提交续跑请求并进入运行态
    assert!(matches!(&effects[..], [Effect::Continue]));
    assert!(app.running);
    assert_eq!(app.chat.items.len(), 1);
    assert!(matches!(app.chat.items[0], ChatItem::User(_)));
}

#[test]
fn continue_pops_unfinished_assistant_item() {
    // 流协议错误路径：MessageStart 后没有 MessageEnd 的未定稿条目同样移除
    let mut app = app();
    app.handle_event(&AgentEvent::MessageStart(user_message("hi")));
    app.handle_event(&AgentEvent::MessageStart(assistant_message(
        Vec::new(),
        StopReason::Stop,
        None,
    )));

    let effects = app.execute_command(CommandAction::Continue);

    assert!(matches!(&effects[..], [Effect::Continue]));
    assert_eq!(app.chat.items.len(), 1);
    assert!(matches!(app.chat.items[0], ChatItem::User(_)));
}

#[test]
fn continue_after_success_keeps_items_and_delegates() {
    // 是否可续跑由 agent 判定（历史是唯一权威）：成功条目保留，照常提交
    let mut app = app();
    app.handle_event(&AgentEvent::MessageStart(user_message("hi")));
    app.handle_event(&AgentEvent::MessageStart(assistant_message(
        vec![text_block("ok")],
        StopReason::Stop,
        None,
    )));
    app.handle_event(&AgentEvent::MessageEnd {
        message: assistant_message(vec![text_block("ok")], StopReason::Stop, None),
        context_tokens: 0,
    });

    let effects = app.execute_command(CommandAction::Continue);

    assert!(matches!(&effects[..], [Effect::Continue]));
    assert_eq!(app.chat.items.len(), 2);
}

#[test]
fn parse_command_skill_uses_colon_argument() {
    let skill = |name: &str, args: Option<&str>| {
        CommandParse::Known(CommandAction::Skill(Some(SkillInvocation {
            name: name.to_string(),
            args: args.map(str::to_string),
        })))
    };
    assert_eq!(
        parse_command("skill"),
        CommandParse::Known(CommandAction::Skill(None))
    );
    assert_eq!(parse_command("skill:jujutsu"), skill("jujutsu", None));
    // 空参数等价于无参（列出清单）
    assert_eq!(
        parse_command("skill:"),
        CommandParse::Known(CommandAction::Skill(None))
    );
    // 名称后首个空白起为附带 args（可为含空格的自由文本）
    assert_eq!(
        parse_command("skill:review 只看 unsafe 块"),
        skill("review", Some("只看 unsafe 块"))
    );
    // `skill name` 空白形式仍属于非法用法（避免与 prompt template 调用混淆）
    assert!(matches!(
        parse_command("skill jujutsu"),
        CommandParse::InvalidUsage(_)
    ));
    // 无参命令带参数同样报用法错误
    assert!(matches!(
        parse_command("new x"),
        CommandParse::InvalidUsage(_)
    ));
    assert!(matches!(
        parse_command("goal x"),
        CommandParse::InvalidUsage(_)
    ));
    assert!(matches!(
        parse_command("resume:abc"),
        CommandParse::InvalidUsage(_)
    ));
    assert!(matches!(
        parse_command("quit:now"),
        CommandParse::InvalidUsage(_)
    ));
    // 未知命令带冒号参数仍报未知
    assert_eq!(
        parse_command("foo:bar"),
        CommandParse::Unknown("foo".to_string())
    );
}

#[test]
fn parse_command_compact_takes_free_text_instructions() {
    assert_eq!(
        parse_command("compact"),
        CommandParse::Known(CommandAction::Compact(None))
    );
    // 空白分隔的自由文本（可含空格）
    assert_eq!(
        parse_command("compact 专注 测试 部分"),
        CommandParse::Known(CommandAction::Compact(Some("专注 测试 部分".to_string())))
    );
    // 冒号形式同样接受
    assert_eq!(
        parse_command("compact:focus on tests"),
        CommandParse::Known(CommandAction::Compact(Some("focus on tests".to_string())))
    );
    // 空参数等价于无参
    assert_eq!(
        parse_command("compact "),
        CommandParse::Known(CommandAction::Compact(None))
    );
    // 前缀不等于命令名：compactx 报未知
    assert_eq!(
        parse_command("compactx"),
        CommandParse::Unknown("compactx".to_string())
    );
}

#[test]
fn parse_command_image_takes_path_argument() {
    assert_eq!(
        parse_command("image:pic.png"),
        CommandParse::Known(CommandAction::Image("pic.png".to_string()))
    );
    // 空白分隔形式同样接受（路径可含空格）
    assert_eq!(
        parse_command("image my pics/a.png"),
        CommandParse::Known(CommandAction::Image("my pics/a.png".to_string()))
    );
    // 无参数报用法
    assert!(matches!(
        parse_command("image"),
        CommandParse::InvalidUsage(_)
    ));
    assert!(matches!(
        parse_command("image "),
        CommandParse::InvalidUsage(_)
    ));
    // 前缀不等于命令名：imagex 报未知
    assert_eq!(
        parse_command("imagex"),
        CommandParse::Unknown("imagex".to_string())
    );
}

#[test]
fn staged_attachments_follow_next_prompt() {
    let mut app = app();
    let image = || nomic_ai::ImageContent {
        data: "aA==".to_string(),
        mime_type: "image/png".to_string(),
    };
    assert!(!app.input.has_attachments());
    assert_eq!(app.input.stage_image("a.png".to_string(), image()), 1);
    assert_eq!(app.input.stage_image("b.png".to_string(), image()), 2);
    assert!(app.input.has_attachments());
    let taken = app.input.take_attachments();
    assert_eq!(taken.len(), 2);
    assert!(!app.input.has_attachments());
    // 取空后再次取出为空
    assert!(app.input.take_attachments().is_empty());
}

#[test]
fn user_message_with_images_shows_placeholder() {
    let message = UserMessageContent::Blocks(vec![
        UserContent::Image(nomic_ai::ImageContent {
            data: "aA==".to_string(),
            mime_type: "image/png".to_string(),
        }),
        UserContent::Text(TextContent {
            text: "描述这张图".to_string(),
            text_signature: None,
        }),
    ]);
    assert_eq!(user_text(&message), "🖼 图片 ×1\n描述这张图");
    // 纯文本块列表不加占位行
    let text_only = UserMessageContent::Blocks(vec![UserContent::Text(TextContent {
        text: "hi".to_string(),
        text_signature: None,
    })]);
    assert_eq!(user_text(&text_only), "hi");
}

mod mention_tests;
mod normal_tests;
mod picker_tests;
fn image() -> nomic_ai::ImageContent {
    nomic_ai::ImageContent {
        data: "aA==".to_string(),
        mime_type: "image/png".to_string(),
    }
}

fn template(name: &str, body: &str, argument_hint: Option<&str>) -> PromptTemplate {
    PromptTemplate {
        name: name.to_string(),
        path: PathBuf::from(format!("/repo/.nomic/prompts/{name}.md")),
        scope: nomic_prompts::PromptScope::Project,
        description: format!("{name} desc"),
        argument_hint: argument_hint.map(str::to_string),
        body: body.to_string(),
    }
}

fn queued_app() -> App {
    let mut app = app();
    app.handle_event(&AgentEvent::AgentStart);
    app.input.stage_image("a.png".to_string(), image());
    app.paste_text("first");
    app.press(Key::Enter);
    app.paste_text("second\n两行");
    app.press(Key::Enter);
    app.finish_run(None);
    assert_eq!(app.queue.len(), 2);
    app
}

fn app_with_history() -> App {
    let mut app = app();
    app.load_history(&[
        *user_message("第一个问题"),
        *assistant_message(vec![text_block("第一个回答")], StopReason::Stop, None),
    ]);
    app.handle_event(&AgentEvent::ToolExecutionStart {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        args: serde_json::json!({"command": "ls"}),
    });
    app.handle_event(&AgentEvent::ToolExecutionEnd {
        tool_call_id: "t1".to_string(),
        tool_name: "bash".to_string(),
        result: ToolResult::text("file.rs"),
        is_error: false,
    });
    app.load_history(&[
        *user_message("第二个问题"),
        *assistant_message(
            vec![text_block(
                "看这里：\n```rust\nfn main() {}\n```\n还有：\n```\n第二块\n```",
            )],
            StopReason::Stop,
            None,
        ),
    ]);
    app
}

mod queue_tests;
