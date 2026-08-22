//! TUI 渲染：组合根与各区域自定义 widget（ratatui）。
//!
//! [`draw`] 是组合根：布局（聊天区 + 运行提示行 + 输入框 + 状态栏）后把各区域交给
//! 对应 widget 渲染——聊天区 [`ChatView`]（只读；几何在渲染前由
//! [`App::sync_chat_geometry`] 按视口算进状态层）、运行状态提示
//! [`RunHint`]（仅运行中占一行：阶段文案 + 扫光动效，空闲时高度为 0）、
//! 输入框 [`InputArea`]、状态栏 [`StatusBar`]，以及覆盖层：COMMAND 模式的浮层命令栏
//! [`CommandPalette`]（不占布局）、选择器弹层 [`PickerPopup`] 与
//! 帮助/提问弹层（[`HelpOverlay`] / [`QuestionOverlay`]，内容区画布居中），
//! 输入框上方的 `@` mention 弹层 [`MentionPopup`]（贴输入框，服务草稿
//! 补全）。各 widget 实现见同名子模块。

mod chat;
mod input;
pub(in crate::tui) mod message;
mod overlay;
mod palette;
mod popup;
mod question;
mod runhint;
mod status;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    widgets::Clear,
};

use crate::tui::app::{App, Mode};

use chat::ChatView;
use input::InputArea;
use overlay::HelpOverlay;
use palette::CommandPalette;
use popup::{MentionPopup, PickerPopup};
use question::QuestionOverlay;
use runhint::RunHint;
use status::StatusBar;

pub(in crate::tui) use status::format_tokens;

/// 绘制整帧。
pub(super) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    // 运行中在输入框上方插入一行运行状态提示（扫光动效）；空闲时高度
    // 为 0，不占布局
    let hint_height = u16::from(app.is_running());
    let chunks = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(hint_height),
        Constraint::Length(input::input_height(app)),
        Constraint::Length(1),
    ])
    .split(frame.area());
    // Paragraph 不清理文本以外的单元格；先 Clear 避免长行残留
    frame.render_widget(Clear, frame.area());

    // 聊天区：渲染前按已知视口把几何（条目起始行/滚动上限）算进状态层，
    // ChatView 只读渲染，不再有渲染期回写
    let chat_area = chunks[0].inner(Margin::new(chat::CHAT_H_MARGIN, 0));
    app.sync_chat_geometry(chat_area.width, chat_area.height);
    frame.render_widget(ChatView::new(app), chat_area);

    // 运行状态提示行（输入框上方；空闲时高度为 0）
    frame.render_widget(RunHint::new(app), chunks[1]);

    // 输入框（COMMAND 下草稿照常渲染，焦点在浮层命令栏）
    let input_widget = InputArea::new(app);
    let input_cursor = input_widget.cursor_position(chunks[2]);
    frame.render_widget(input_widget, chunks[2]);

    // 浮层命令栏（COMMAND，ADR-0020 修订）：屏幕中上方的覆盖层单行
    // 输入框，不占布局；光标随之移到栏内。选择器打开时输入框失焦，
    // 光标由选择器弹层自身设置（过滤输入行），不落回输入框
    if app.mode() == Mode::Command {
        let palette = CommandPalette::new(app.command());
        let cursor = palette.cursor_position(frame.area());
        frame.render_widget(palette, frame.area());
        frame.set_cursor_position(cursor);
    } else if app.picker().is_none() {
        frame.set_cursor_position(input_cursor);
    }

    // 状态栏
    frame.render_widget(StatusBar::new(app), chunks[3]);

    // `@` mention 补全弹层贴输入框上方（服务草稿补全，ADR-0020）；
    // 选择器/帮助/提问弹层是模态覆盖层：内容区（状态栏以上）整体作为画布
    if let Some(mention) = app.input().mention() {
        frame.render_widget(MentionPopup::new(mention), chunks[2]);
    }
    let content = Rect {
        height: frame.area().height.saturating_sub(1),
        ..frame.area()
    };
    // 选择器弹层（resume/models/tree/思考级别）：居中浮层，不再贴输入框；
    // 光标落在弹层内的过滤输入行
    if let Some(picker) = app.picker() {
        let popup = PickerPopup::new(picker);
        let cursor = popup.cursor_position(content);
        frame.render_widget(popup, content);
        frame.set_cursor_position(cursor);
    }
    // 帮助弹层是模态覆盖层：内容区（状态栏以上）整体作为画布
    if app.help_open()
        && let Some(scroll) = app.help_scroll_mut()
    {
        frame.render_stateful_widget(HelpOverlay, content, scroll);
    }
    // 提问弹层（ADR-0029）：模态覆盖层，最后渲染（最上层）；自定义输入
    // 阶段把终端光标放进弹层内的输入行（列表阶段光标形状归 block_cursor）
    if let Some(question) = app.question() {
        let overlay = QuestionOverlay::new(question);
        let position = overlay.cursor_position(content);
        frame.render_widget(overlay, content);
        if let Some(position) = position {
            frame.set_cursor_position(position);
        }
    }
}

#[cfg(test)]
mod tests {
    use nomic_ai::Message;
    use nomic_core::{AgentEvent, ToolResult};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    /// 提取 buffer 全文（去空白），便于断言可见内容。
    fn compact_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .flat_map(|cell| cell.symbol().chars())
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    /// 渲染一帧并提取全部非空白字符，供状态栏断言用。
    fn render_compact(app: &mut App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        compact_text(&terminal)
    }

    /// 构造一条含两行 thinking 的 assistant 消息。
    fn thinking_app() -> App {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::Assistant(
            nomic_ai::AssistantMessage {
                content: Vec::new(),
                api: nomic_ai::ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude".to_string(),
                response_model: None,
                response_id: None,
                usage: nomic_ai::Usage::default(),
                stop_reason: nomic_ai::StopReason::Stop,
                error_message: None,
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::ThinkingStart { index: 0 },
        ));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::ThinkingDelta {
                index: 0,
                delta: "推理第一行\n推理第二行".to_string(),
            },
        ));
        app
    }

    /// 渲染冒烟：空状态、流式中、工具条目三种画面都能无 panic 绘制。
    #[test]
    fn renders_without_panic() {
        let mut app = App::new(
            "test-model".to_string(),
            Some("abcd1234-session".to_string()),
            200_000,
        );
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::User(
            nomic_ai::UserMessage {
                content: nomic_ai::UserMessageContent::Text("你好".to_string()),
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({"command": "ls -la"}),
        });
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let compact = compact_text(&terminal);
        assert!(compact.contains("你好"));
        assert!(compact.contains("bash"));
        assert!(compact.contains("test-model"));
        // session id 是内部标识，不对用户展示
        assert!(!compact.contains("abcd1234"));
    }

    /// 运行中：状态提示由输入框上方的提示行承担（waiting...），输入框
    /// 标题不再叠加 spinner 与「运行中」字样；聊天区亦不叠加流式指示。
    #[test]
    fn shows_running_input_state() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::Assistant(
            nomic_ai::AssistantMessage {
                content: Vec::new(),
                api: nomic_ai::ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude".to_string(),
                response_model: None,
                response_id: None,
                usage: nomic_ai::Usage::default(),
                stop_reason: nomic_ai::StopReason::Stop,
                error_message: None,
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::AgentStart);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let compact = compact_text(&terminal);
        assert!(!compact.contains("生成中"), "{compact}");
        assert!(!compact.contains("运行中"), "{compact}");
        assert!(!compact.contains(app.spinner()), "{compact}");
        assert!(compact.contains("waiting..."), "{compact}");
    }

    /// 运行状态提示行（输入框上方）：阶段文案随聊天区尾部切换；扫光带
    /// 头部字符带最亮样式；运行结束（空闲）后提示行不占布局。
    #[test]
    fn shows_run_phase_hint_with_shimmer() {
        use nomic_ai::AssistantEvent;
        use ratatui::style::Modifier;

        let mut app = thinking_app();
        app.handle_event(&AgentEvent::AgentStart);
        app.tick();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let compact = compact_text(&terminal);
        assert!(compact.contains("thinking..."), "{compact}");
        // 扫光头部字符带加粗样式（tick=1：头部在第 2 字符）
        let head = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "h")
            .expect("thinking 字符 cell");
        assert!(head.modifier.contains(Modifier::BOLD));

        // 正文流式输出：提示切换为 writing...
        app.handle_event(&AgentEvent::MessageUpdate(AssistantEvent::TextStart {
            index: 1,
        }));
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        assert!(compact.contains("writing..."), "{compact}");

        // 运行结束：提示行消失（不占布局）
        app.finish_run(None);
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        assert!(!compact.contains("writing..."), "{compact}");
        assert!(!compact.contains("waiting..."), "{compact}");
    }

    /// 队列区渲染（ADR-0014）：排队消息显示在输入框草稿上方，运行中
    /// 标题显示条数；QUEUE 模式标题切换、游标条目以 `❯` gutter 标出。
    #[test]
    fn renders_queue_area_and_queue_mode_title() {
        use super::super::app::Key;

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::AgentStart);
        app.paste_text("第一条");
        app.press(Key::Enter);
        app.paste_text("第二条");
        app.press(Key::Enter);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        // 运行中标题显示排队条数；条目以 `»` gutter 标出
        assert!(compact.contains("2条排队"), "{compact}");
        assert!(compact.contains("»第一条"), "{compact}");
        assert!(compact.contains("»第二条"), "{compact}");

        // 异常结束后空闲：标题提示队列暂停与恢复方式
        app.finish_run(Some("已取消".to_string()));
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        assert!(compact.contains("队列暂停2条"), "{compact}");

        // QUEUE 模式：徽标、标题与游标条目 gutter（❯）
        app.press(Key::Esc);
        app.press(Key::Char('m'));
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        assert!(compact.contains("QUEUE"), "{compact}");
        assert!(compact.contains("消息队列2条"), "{compact}");
        assert!(compact.contains("❯第一条"), "{compact}");
        assert!(compact.contains("»第二条"), "{compact}");
    }

    /// 工具条目树形渲染：参数用语义摘要，多行结果首行 `⎿`、后续行对齐。
    #[test]
    fn renders_tool_tree_with_multiline_detail() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({"command": "cargo test"}),
        });
        app.handle_event(&AgentEvent::ToolExecutionEnd {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            result: ToolResult::text("line1\nline2\nline3"),
            is_error: false,
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let rows: Vec<String> = buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect())
            .collect();
        let text = rows.join("\n");
        assert!(text.contains("bash(cargo test)"), "{text}");
        assert!(text.contains("⎿ line1"), "{text}");
        assert!(text.contains("  line2"), "{text}");
        assert!(text.contains("  line3"), "{text}");
    }

    /// 各条目共用 `▌` gutter 组件，但颜色不同：用户=accent，工具=状态色。
    #[test]
    fn gutter_colors_distinguish_item_types() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::User(
            nomic_ai::UserMessage {
                content: nomic_ai::UserMessageContent::Text("用户消息".to_string()),
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::ToolExecutionStart {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({"command": "ls"}),
        });
        app.handle_event(&AgentEvent::ToolExecutionEnd {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            result: ToolResult::text("ok"),
            is_error: false,
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let gutter_colors: Vec<ratatui::style::Color> = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "▌")
            .map(|cell| cell.fg)
            .collect();
        assert!(
            gutter_colors.contains(&ratatui::style::Color::Cyan),
            "{gutter_colors:?}"
        );
        assert!(
            gutter_colors.contains(&ratatui::style::Color::Green),
            "{gutter_colors:?}"
        );
    }

    /// System 条目与错误/流式状态行也套用 gutter 组件：System=暗色，错误=红色。
    #[test]
    fn system_and_error_lines_use_gutter() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.chat_mut().push_system("本地系统提示");
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::Assistant(
            nomic_ai::AssistantMessage {
                content: Vec::new(),
                api: nomic_ai::ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude".to_string(),
                response_model: None,
                response_id: None,
                usage: nomic_ai::Usage::default(),
                stop_reason: nomic_ai::StopReason::Stop,
                error_message: None,
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::MessageEnd {
            message: Box::new(Message::Assistant(nomic_ai::AssistantMessage {
                content: Vec::new(),
                api: nomic_ai::ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude".to_string(),
                response_model: None,
                response_id: None,
                usage: nomic_ai::Usage::default(),
                stop_reason: nomic_ai::StopReason::Error,
                error_message: Some("rate limited".to_string()),
                timestamp: 0,
            })),
            context_tokens: 0,
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let compact = compact_text(&terminal);
        assert!(compact.contains("▌本地系统提示"), "{compact}");
        assert!(compact.contains("▌✗ratelimited"), "{compact}");
        let gutter_colors: Vec<ratatui::style::Color> = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "▌")
            .map(|cell| cell.fg)
            .collect();
        assert!(
            gutter_colors.contains(&ratatui::style::Color::DarkGray),
            "{gutter_colors:?}"
        );
        assert!(
            gutter_colors.contains(&ratatui::style::Color::Red),
            "{gutter_colors:?}"
        );
    }

    /// thinking 块套用 gutter 组件：带标题与树形前缀，默认折叠（仅占位行），
    /// `thinking` 命令展开后逐行渲染正文。
    #[test]
    fn renders_thinking_block_with_gutter() {
        let mut app = thinking_app();

        // 默认折叠：不渲染正文行，只渲染标题与折叠提示
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        assert!(!compact.contains("推理第一行"), "{compact}");
        assert!(!compact.contains("推理第二行"), "{compact}");
        assert!(compact.contains("▌思考（2行，已折叠）"), "{compact}");

        // `thinking` 命令（命令栏，ADR-0020）展开后渲染标题与正文行
        app.press(super::super::app::Key::Esc);
        app.press(super::super::app::Key::Char(':'));
        app.paste_text("thinking");
        app.press(super::super::app::Key::Enter);
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");
        let compact = compact_text(&terminal);
        assert!(compact.contains("▌思考"), "{compact}");
        assert!(compact.contains("▌⎿推理第一行"), "{compact}");
        assert!(compact.contains("▌推理第二行"), "{compact}");
    }

    /// 空状态绘制欢迎页：logo、模型名与键位速查均可见。
    #[test]
    fn renders_welcome_when_empty() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let compact = compact_text(&terminal);
        assert!(compact.contains("nomic"), "{compact}");
        assert!(compact.contains("test-model"), "{compact}");
        assert!(compact.contains("命令栏（help）"), "{compact}");
    }

    /// NORMAL：状态栏显示模式徽标与浏览键位提示。
    #[test]
    fn renders_normal_mode_badge() {
        use super::super::app::Key;
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::User(
            nomic_ai::UserMessage {
                content: nomic_ai::UserMessageContent::Text("你好".to_string()),
                timestamp: 0,
            },
        ))));
        app.press(Key::Esc);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let compact = compact_text(&terminal);
        assert!(compact.contains("NORMAL"), "{compact}");
    }

    /// assistant 文本块按 Markdown 渲染：标记符号不原样上屏，样式落到 cell。
    #[test]
    fn renders_assistant_markdown_with_styles() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::Assistant(
            nomic_ai::AssistantMessage {
                content: Vec::new(),
                api: nomic_ai::ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude".to_string(),
                response_model: None,
                response_id: None,
                usage: nomic_ai::Usage::default(),
                stop_reason: nomic_ai::StopReason::Stop,
                error_message: None,
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::TextStart { index: 0 },
        ));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::TextDelta {
                index: 0,
                delta: "# 标题\n\n- 项一\n- 项二".to_string(),
            },
        ));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let compact = compact_text(&terminal);
        // Markdown 标记不原样上屏，列表项带渲染后的符号
        assert!(compact.contains("标题"), "{compact}");
        assert!(!compact.contains("#标题"), "{compact}");
        assert!(compact.contains("•项一"), "{compact}");
        // 标题 cell 带加粗样式
        let heading_cell = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "标")
            .expect("heading cell");
        assert!(
            heading_cell
                .modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    /// 聊天区左右留白：assistant 输出不紧贴屏幕左缘。
    #[test]
    fn chat_content_has_left_margin() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.handle_event(&AgentEvent::MessageStart(Box::new(Message::Assistant(
            nomic_ai::AssistantMessage {
                content: Vec::new(),
                api: nomic_ai::ApiKind::AnthropicMessages,
                provider: "anthropic".to_string(),
                model: "claude".to_string(),
                response_model: None,
                response_id: None,
                usage: nomic_ai::Usage::default(),
                stop_reason: nomic_ai::StopReason::Stop,
                error_message: None,
                timestamp: 0,
            },
        ))));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::TextStart { index: 0 },
        ));
        app.handle_event(&AgentEvent::MessageUpdate(
            nomic_ai::AssistantEvent::TextDelta {
                index: 0,
                delta: "输出内容".to_string(),
            },
        ));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let index = buffer
            .content()
            .iter()
            .position(|cell| cell.symbol() == "输")
            .expect("assistant text cell");
        let x = u16::try_from(index % width).expect("column fits u16");
        // assistant 输出套用 gutter 组件：留白 + `▌ ` 竖条两列
        assert_eq!(
            x,
            chat::CHAT_H_MARGIN + 2,
            "assistant 输出应距左缘 {} 列",
            chat::CHAT_H_MARGIN + 2
        );
    }

    /// 浮层命令栏（COMMAND，ADR-0020 修订）：屏幕中上方的覆盖层单行
    /// 输入框（`:` 提示符 + 候选列表）；草稿与聊天区保持可见。
    #[test]
    fn renders_command_palette_overlay() {
        use super::super::app::Key;

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.chat_mut().push_system("本地系统提示");
        app.paste_text("未发送的草稿");
        app.press(Key::Esc);
        app.press(Key::Char(':'));
        app.paste_text("n");
        assert!(app.command().completion().is_some());

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let compact = compact_text(&terminal);
        // 命令栏（无前缀候选）、草稿与 System 条目均可见
        assert!(compact.contains(":n"), "{compact}");
        assert!(compact.contains("new"), "{compact}");
        assert!(compact.contains("未发送的草稿"), "{compact}");
        assert!(compact.contains("本地系统提示"), "{compact}");
    }

    /// 聊天区拼接：相邻消息块之间恰好空一行（工具/System/用户等任意组合）。
    #[test]
    fn chat_blocks_are_separated_by_blank_lines() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.chat_mut().push_system("第一条");
        app.chat_mut().push_system("第二条");

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        let row_of = |needle: &str| {
            buffer
                .content()
                .iter()
                .position(|cell| cell.symbol() == needle)
                .map(|index| index / width)
        };
        let first = row_of("第").expect("first system row");
        let second = row_of("二").expect("second system row");
        // 两个单行消息块之间恰好一行空白
        assert_eq!(second - first, 2, "相邻消息块之间应空一行");
        // 分隔行是真空行：整行无内容字符
        let blank_row = &buffer.content()[(first + 1) * width..(first + 2) * width];
        assert!(
            blank_row.iter().all(|cell| cell.symbol() == " "),
            "分隔行应为空白行"
        );
    }

    /// 状态栏上下文用量：常态只显示紧凑占比；逼近窗口（≥80%）时展开完整数值。
    #[test]
    fn status_bar_shows_context_usage() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.set_context_tokens(8_432);
        let compact = render_compact(&mut app);
        assert!(compact.contains("ctx4%"), "{compact}");
        assert!(!compact.contains("8.4k/200k"), "{compact}");

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.set_context_tokens(168_000);
        let compact = render_compact(&mut app);
        assert!(compact.contains("ctx168k/200k·84%"), "{compact}");
    }

    /// 状态栏模式徽标：INSERT 低调徽标；进入 NORMAL 切换为强提示徽标。
    #[test]
    fn status_bar_badge_follows_mode() {
        use super::super::app::Key;

        // 占位条目避免欢迎页（其键位简介提到 NORMAL）干扰徽标断言
        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.chat_mut().push_system("占位");
        let compact = render_compact(&mut app);
        assert!(compact.contains("INSERT"), "{compact}");
        assert!(!compact.contains("NORMAL"), "{compact}");

        let _ = app.press(Key::Esc);
        let compact = render_compact(&mut app);
        assert!(compact.contains("NORMAL"), "{compact}");
        assert!(!compact.contains("INSERT"), "{compact}");

        // NORMAL `:` 进入 COMMAND：徽标与键位提示切换（ADR-0020）
        let _ = app.press(Key::Char(':'));
        let compact = render_compact(&mut app);
        assert!(compact.contains("COMMAND"), "{compact}");
        assert!(!compact.contains("NORMAL"), "{compact}");
    }

    /// 帮助弹层（NORMAL `?`）：渲染分组键位表与 HELP 徽标，Esc 关闭；
    /// 终端高度不足时 G 滚动到底可见末尾分组。
    #[test]
    fn renders_help_overlay() {
        use super::super::app::Key;

        let render_sized = |app: &mut App, width: u16, height: u16| {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).expect("terminal");
            terminal.draw(|frame| draw(frame, app)).expect("draw");
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .flat_map(|cell| cell.symbol().chars())
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        };

        let mut app = App::new("test-model".to_string(), None, 200_000);
        app.press(Key::Esc);
        app.press(Key::Char('?'));
        // 足够高：全部分组可见
        let compact = render_sized(&mut app, 100, 60);
        assert!(compact.contains("HELP"), "{compact}");
        assert!(compact.contains("键位帮助"), "{compact}");
        assert!(compact.contains("Ctrl+G"), "{compact}");
        assert!(compact.contains("队列编辑"), "{compact}");

        // 高度不足：末尾分组先不可见，G 滚动到底后可见
        let compact = render_sized(&mut app, 100, 20);
        assert!(!compact.contains("队列编辑"), "{compact}");
        app.press(Key::Char('G'));
        let compact = render_sized(&mut app, 100, 20);
        assert!(compact.contains("队列编辑"), "{compact}");

        app.press(Key::Esc);
        let compact = render_sized(&mut app, 100, 20);
        assert!(!compact.contains("HELP"), "{compact}");
    }
}
