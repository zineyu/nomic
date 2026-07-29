//! TUI 状态层：聊天条目、流式增量累积、输入编辑、滚动。
//!
//! 本模块不碰终端，全部逻辑可脱离 ratatui/crossterm 单测。

use nomic_ai::{
    AssistantContent, AssistantEvent, Message, StopReason, UserContent, UserMessageContent,
};
use nomic_core::AgentEvent;
use unicode_width::UnicodeWidthStr;

use crate::print::brief_args;

/// 聊天区条目。
#[derive(Debug)]
pub(super) enum ChatItem {
    /// 用户消息
    User(String),
    /// assistant 消息（流式中逐步累积）
    Assistant(AssistantItem),
    /// 一次工具执行
    Tool(ToolItem),
    /// 本地系统提示（slash 命令输出等，不进上下文）
    System(String),
}

/// assistant 消息条目：有序内容块 + 定稿状态。
#[derive(Debug, Default)]
pub(super) struct AssistantItem {
    pub(super) blocks: Vec<Block>,
    pub(super) done: bool,
    /// `stop_reason` 为 Error/Aborted 时的错误信息
    pub(super) error: Option<String>,
}

/// assistant 内容块（工具调用块不进聊天区，由 `ToolExecution*` 事件承载）。
#[derive(Debug)]
pub(super) enum Block {
    Text(String),
    Thinking(String),
}

/// 工具执行状态。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ToolStatus {
    Running,
    Ok,
    Failed,
}

/// 工具执行条目。
#[derive(Debug)]
pub(super) struct ToolItem {
    /// 工具调用 id（并行执行时按 id 匹配 update/end）
    pub(super) id: String,
    pub(super) name: String,
    /// 参数摘要（截断）
    pub(super) args: String,
    pub(super) status: ToolStatus,
    /// 进度/结果的一行摘要
    pub(super) detail: Option<String>,
}

/// 一条 slash 命令的静态描述。
#[derive(Debug)]
pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) aliases: &'static [&'static str],
    pub(super) summary: &'static str,
}

/// 全部 slash 命令（补全候选与 `/help` 输出的唯一来源）。
pub(super) const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        aliases: &[],
        summary: "显示可用命令",
    },
    SlashCommand {
        name: "new",
        aliases: &[],
        summary: "清空上下文，开启新对话（新 session）",
    },
    SlashCommand {
        name: "quit",
        aliases: &["exit"],
        summary: "退出 TUI",
    },
];

/// slash 命令解析结果。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SlashParse {
    /// 输入不以 `/` 开头，按普通 prompt 处理
    NotCommand,
    /// 已知命令
    Known(SlashAction),
    /// 未知命令名（不含 `/` 前缀）
    Unknown(String),
}

/// 已知 slash 命令的动作。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SlashAction {
    Help,
    New,
    Quit,
}

/// 解析一行输入为 slash 命令。
pub(super) fn parse_slash(input: &str) -> SlashParse {
    let Some(rest) = input.trim().strip_prefix('/') else {
        return SlashParse::NotCommand;
    };
    // 最小版命令均无参数；带参数输入取首个空白前的命令名
    let name = rest.split_whitespace().next().unwrap_or_default();
    for command in SLASH_COMMANDS {
        if command.name == name || command.aliases.contains(&name) {
            return SlashParse::Known(match command.name {
                "help" => SlashAction::Help,
                "new" => SlashAction::New,
                _ => SlashAction::Quit,
            });
        }
    }
    SlashParse::Unknown(name.to_string())
}

/// `/help` 的输出文本。
pub(super) fn help_text() -> String {
    use std::fmt::Write as _;
    let mut text = "可用命令：".to_string();
    for command in SLASH_COMMANDS {
        let aliases = if command.aliases.is_empty() {
            String::new()
        } else {
            let list = command
                .aliases
                .iter()
                .map(|alias| format!("/{alias}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("（别名：{list}）")
        };
        let _ = write!(
            text,
            "\n  /{} — {}{}",
            command.name, command.summary, aliases
        );
    }
    text
}

/// 补全弹层状态：候选列表 + 当前选中项。
#[derive(Debug)]
pub(super) struct Completion {
    pub(super) candidates: Vec<&'static SlashCommand>,
    pub(super) selected: usize,
}

/// TUI 应用状态。
#[derive(Debug)]
pub(super) struct App {
    pub(super) items: Vec<ChatItem>,
    /// 输入缓冲（单行）
    input: String,
    /// 光标位置（字节索引，始终落在 char 边界）
    cursor: usize,
    /// slash 命令补全弹层（输入以 `/` 开头时出现）
    completion: Option<Completion>,
    /// 从底部向上滚动的行数（0 = 跟随最新内容）
    pub(super) scroll: u16,
    pub(super) running: bool,
    pub(super) should_quit: bool,
    /// 模型展示名
    pub(super) model_name: String,
    /// 当前 session id（未持久化时为 None）
    pub(super) session_id: Option<String>,
    /// 状态栏一次性提示（告警等）
    pub(super) notice: Option<String>,
}

impl App {
    pub(super) const fn new(model_name: String, session_id: Option<String>) -> Self {
        Self {
            items: Vec::new(),
            input: String::new(),
            cursor: 0,
            completion: None,
            scroll: 0,
            running: false,
            should_quit: false,
            model_name,
            session_id,
            notice: None,
        }
    }

    /// 把 resume 恢复的历史消息渲染为聊天条目。
    pub(super) fn load_history(&mut self, messages: &[Message]) {
        for message in messages {
            match message {
                Message::User(user) => self.items.push(ChatItem::User(user_text(&user.content))),
                Message::Assistant(assistant) => {
                    let error =
                        assistant_error(assistant.stop_reason, assistant.error_message.as_deref());
                    self.items.push(ChatItem::Assistant(AssistantItem {
                        blocks: assistant
                            .content
                            .iter()
                            .filter_map(|content| match content {
                                AssistantContent::Text(text) => {
                                    Some(Block::Text(text.text.clone()))
                                }
                                AssistantContent::Thinking(thinking) => {
                                    Some(Block::Thinking(thinking.thinking.clone()))
                                }
                                AssistantContent::ToolCall(_) => None,
                            })
                            .collect(),
                        done: true,
                        error,
                    }));
                }
                Message::ToolResult(result) => {
                    self.items.push(ChatItem::Tool(ToolItem {
                        id: result.tool_call_id.clone(),
                        name: result.tool_name.clone(),
                        args: String::new(),
                        status: if result.is_error {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Ok
                        },
                        detail: result_summary(&result.content),
                    }));
                }
            }
        }
    }

    /// 消费一个 agent 事件，更新状态。
    pub(super) fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart => self.running = true,
            AgentEvent::MessageStart(message) => match message.as_ref() {
                Message::User(user) => {
                    self.items.push(ChatItem::User(user_text(&user.content)));
                    self.scroll_to_bottom();
                }
                Message::Assistant(_) => {
                    self.items
                        .push(ChatItem::Assistant(AssistantItem::default()));
                }
                Message::ToolResult(_) => {}
            },
            AgentEvent::MessageUpdate(delta) => self.apply_delta(delta),
            AgentEvent::MessageEnd(message) => {
                if let Message::Assistant(assistant) = message.as_ref() {
                    if let Some(ChatItem::Assistant(item)) = self.items.last_mut() {
                        item.done = true;
                        item.error = assistant_error(
                            assistant.stop_reason,
                            assistant.error_message.as_deref(),
                        );
                    }
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                self.items.push(ChatItem::Tool(ToolItem {
                    id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    args: brief_args(args),
                    status: ToolStatus::Running,
                    detail: None,
                }));
                self.scroll_to_bottom();
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial,
                ..
            } => {
                if let Some(tool) = self.find_tool_mut(tool_call_id) {
                    tool.detail = result_summary(&partial.content);
                }
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => {
                if let Some(tool) = self.find_tool_mut(tool_call_id) {
                    tool.status = if *is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Ok
                    };
                    tool.detail = result_summary(&result.content);
                }
            }
            AgentEvent::AgentEnd { .. } | AgentEvent::TurnStart | AgentEvent::TurnEnd { .. } => {}
        }
    }

    /// 按 `(index, delta)` 累积流式 assistant 内容（ADR-0001 消费方义务）。
    fn apply_delta(&mut self, delta: &AssistantEvent) {
        let Some(ChatItem::Assistant(item)) = self.items.last_mut() else {
            return;
        };
        match delta {
            AssistantEvent::TextStart { index } => {
                insert_block(&mut item.blocks, *index, Block::Text(String::new()));
            }
            AssistantEvent::TextDelta { index, delta } => {
                if let Some(Block::Text(text)) = item.blocks.get_mut(*index) {
                    text.push_str(delta);
                }
            }
            AssistantEvent::ThinkingStart { index } => {
                insert_block(&mut item.blocks, *index, Block::Thinking(String::new()));
            }
            AssistantEvent::ThinkingDelta { index, delta } => {
                if let Some(Block::Thinking(thinking)) = item.blocks.get_mut(*index) {
                    thinking.push_str(delta);
                }
            }
            // End/Done/Error 不携带增量；Done/Error 由 core 转为 MessageEnd，不会到这里
            _ => {}
        }
    }

    fn find_tool_mut(&mut self, tool_call_id: &str) -> Option<&mut ToolItem> {
        self.items.iter_mut().rev().find_map(|item| {
            if let ChatItem::Tool(tool) = item
                && tool.id == tool_call_id
            {
                Some(tool)
            } else {
                None
            }
        })
    }

    // ── 输入编辑 ────────────────────────────────────────────────────────────

    pub(super) fn input(&self) -> &str {
        &self.input
    }

    /// 光标前文本的显示宽度（输入框渲染光标用）。
    pub(super) fn cursor_width(&self) -> u16 {
        u16::try_from(UnicodeWidthStr::width(&self.input[..self.cursor])).unwrap_or(u16::MAX)
    }

    pub(super) fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.refresh_completion();
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.input[..self.cursor]
            .char_indices()
            .last()
            .map_or(0, |(index, _)| index);
        self.input.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.refresh_completion();
    }

    pub(super) fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .last()
                .map_or(0, |(index, _)| index);
            self.refresh_completion();
        }
    }

    pub(super) fn cursor_right(&mut self) {
        if let Some(c) = self.input[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
            self.refresh_completion();
        }
    }

    pub(super) fn cursor_home(&mut self) {
        self.cursor = 0;
        self.refresh_completion();
    }

    pub(super) fn cursor_end(&mut self) {
        self.cursor = self.input.len();
        self.refresh_completion();
    }

    /// 取出待提交的输入并清空缓冲；空输入返回 `None`。
    pub(super) fn take_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor = 0;
        self.completion = None;
        Some(text)
    }

    // ── slash 命令补全 ──────────────────────────────────────────────────────

    /// 当前补全弹层（渲染用）。
    pub(super) const fn completion(&self) -> Option<&Completion> {
        self.completion.as_ref()
    }

    /// 按当前输入重算补全候选：仅在「以 `/` 开头、光标在末尾、命令名
    /// 未输入完整参数（无空白）」时弹出；候选按前缀匹配命令名与别名。
    fn refresh_completion(&mut self) {
        let fragment = self.slash_fragment();
        self.completion = fragment.and_then(|fragment| {
            let mut candidates: Vec<&'static SlashCommand> = SLASH_COMMANDS
                .iter()
                .filter(|command| {
                    command.name.starts_with(fragment)
                        || command.aliases.iter().any(|a| a.starts_with(fragment))
                })
                .collect();
            if candidates.is_empty() {
                return None;
            }
            candidates.sort_by_key(|command| command.name);
            // 输入已精确匹配某命令时选中它，Tab 从它开始循环
            let selected = candidates
                .iter()
                .position(|command| command.name == fragment)
                .unwrap_or(0);
            Some(Completion {
                candidates,
                selected,
            })
        });
    }

    /// 光标位于末尾且输入是「无参数的 slash 前缀」时，返回命令名片段。
    fn slash_fragment(&self) -> Option<&str> {
        let rest = self.input.strip_prefix('/')?;
        if self.cursor != self.input.len() || rest.contains(char::is_whitespace) {
            return None;
        }
        Some(rest)
    }

    /// Tab：接受当前选中候选；输入已等于选中项时循环到下一个。
    pub(super) fn tab_complete(&mut self) {
        let Some(completion) = &self.completion else {
            return;
        };
        let current = completion.candidates[completion.selected];
        let selected = if self.input == format!("/{}", current.name) {
            (completion.selected + 1) % completion.candidates.len()
        } else {
            completion.selected
        };
        let name = completion.candidates[selected].name;
        self.input = format!("/{name}");
        self.cursor = self.input.len();
        self.refresh_completion();
    }

    /// 补全弹层中选择下一个/上一个候选（环形）。
    pub(super) const fn completion_select(&mut self, delta: isize) {
        if let Some(completion) = &mut self.completion {
            let len = completion.candidates.len();
            let step = delta.unsigned_abs() % len;
            completion.selected = if delta < 0 {
                (completion.selected + len - step) % len
            } else {
                (completion.selected + step) % len
            };
        }
    }

    /// Esc：关闭补全弹层；返回是否确有弹层被关闭（否则调用方走取消语义）。
    pub(super) fn dismiss_completion(&mut self) -> bool {
        self.completion.take().is_some()
    }

    /// Enter 且补全弹层可见时的智能接受：输入未精确匹配任何命令时
    /// 填入选中候选（返回 `true`，不提交）；已精确匹配则返回 `false` 正常提交。
    pub(super) fn accept_completion_on_enter(&mut self) -> bool {
        let Some(fragment) = self.slash_fragment() else {
            return false;
        };
        if self.completion.is_none() {
            return false;
        }
        let exact = self.completion.as_ref().is_some_and(|completion| {
            completion
                .candidates
                .iter()
                .any(|command| command.name == fragment || command.aliases.contains(&fragment))
        });
        if exact {
            return false;
        }
        self.tab_complete();
        true
    }

    // ── slash 命令反馈 ──────────────────────────────────────────────────────

    /// 追加一条本地系统提示（不进上下文、不落库）。
    pub(super) fn push_system(&mut self, text: impl Into<String>) {
        self.items.push(ChatItem::System(text.into()));
        self.scroll_to_bottom();
    }

    /// 清空聊天区（`/new` 开启新对话）。
    pub(super) fn clear_items(&mut self) {
        self.items.clear();
        self.scroll_to_bottom();
    }

    // ── 滚动 ────────────────────────────────────────────────────────────────

    pub(super) const fn scroll_up(&mut self, lines: u16) {
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub(super) const fn scroll_down(&mut self, lines: u16) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub(super) const fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }
}

/// 在 `index` 处放置块（provider 按序发出，但容错乱序）。
fn insert_block(blocks: &mut Vec<Block>, index: usize, block: Block) {
    if index <= blocks.len() {
        blocks.insert(index, block);
    }
}

fn user_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(text) => text.clone(),
        UserMessageContent::Blocks(blocks) => blocks_text(blocks),
    }
}

fn blocks_text(blocks: &[UserContent]) -> String {
    blocks
        .iter()
        .filter_map(|content| match content {
            UserContent::Text(text) => Some(text.text.as_str()),
            UserContent::Image(_) => None,
        })
        .collect::<String>()
}

/// 提取工具输出的一行摘要（最后一行非空文本，截断到 80 列）。
fn result_summary(blocks: &[UserContent]) -> Option<String> {
    const MAX: usize = 80;
    let text = blocks_text(blocks);
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    let line = line.trim();
    if line.chars().count() <= MAX {
        return Some(line.to_string());
    }
    let truncated: String = line.chars().take(MAX).collect();
    Some(format!("{truncated}…"))
}

fn assistant_error(stop_reason: StopReason, error_message: Option<&str>) -> Option<String> {
    if matches!(stop_reason, StopReason::Error | StopReason::Aborted) {
        Some(error_message.unwrap_or("未知错误").to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use nomic_ai::{ApiKind, AssistantMessage, TextContent, ThinkingContent, Usage, UserMessage};
    use nomic_core::{ToolResult, ToolUpdate};

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
        App::new("test-model".to_string(), None)
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
        app.handle_event(&AgentEvent::MessageEnd(assistant_message(
            Vec::new(),
            StopReason::Stop,
            None,
        )));

        let Some(ChatItem::Assistant(item)) = app.items.first() else {
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
        app.handle_event(&AgentEvent::MessageEnd(assistant_message(
            Vec::new(),
            StopReason::Error,
            Some("rate limited".to_string()),
        )));
        let Some(ChatItem::Assistant(item)) = app.items.first() else {
            panic!("expected assistant item");
        };
        assert_eq!(item.error.as_deref(), Some("rate limited"));
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

        let Some(ChatItem::Tool(tool)) = app.items.first() else {
            panic!("expected tool item");
        };
        assert_eq!(tool.status, ToolStatus::Ok);
        assert_eq!(tool.detail.as_deref(), Some("done"));
        assert!(tool.args.contains("ls"));
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

        let [ChatItem::Tool(first), ChatItem::Tool(second)] = &app.items[..] else {
            panic!("unexpected items");
        };
        assert_eq!(first.status, ToolStatus::Failed);
        assert_eq!(second.status, ToolStatus::Running);
    }

    #[test]
    fn input_editing_respects_char_boundaries() {
        let mut app = app();
        app.insert_char('你');
        app.insert_char('好');
        app.cursor_left();
        app.insert_char('a');
        assert_eq!(app.input(), "你a好");
        app.backspace();
        assert_eq!(app.input(), "你好");
        app.backspace();
        assert_eq!(app.input(), "好");
        assert_eq!(app.take_input().as_deref(), Some("好"));
        assert!(app.take_input().is_none());
    }

    #[test]
    fn slash_completion_filters_by_prefix_and_tab_cycles() {
        let mut app = app();
        app.insert_char('/');
        let completion = app.completion().expect("/ 即弹出全部候选");
        assert_eq!(completion.candidates.len(), SLASH_COMMANDS.len());

        app.insert_char('n');
        let completion = app.completion().expect("/n 匹配 new");
        assert_eq!(
            completion
                .candidates
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            vec!["new"]
        );

        // Tab 接受候选
        app.tab_complete();
        assert_eq!(app.input(), "/new");
        // 精确匹配后仍显示（展示描述），且选中该项
        let completion = app.completion().expect("精确匹配仍显示候选");
        assert_eq!(completion.candidates[completion.selected].name, "new");

        // 输入空格（进入参数区）后弹层消失
        app.insert_char(' ');
        assert!(app.completion().is_none());
    }

    #[test]
    fn slash_completion_matches_alias_and_enter_accepts() {
        let mut app = app();
        for c in "/ex".chars() {
            app.insert_char(c);
        }
        let completion = app.completion().expect("/ex 匹配别名 exit");
        assert_eq!(completion.candidates[completion.selected].name, "quit");

        // 未精确匹配时 Enter 先填入候选，不提交
        assert!(app.accept_completion_on_enter());
        assert_eq!(app.input(), "/quit");
        // 精确匹配后 Enter 放行提交
        assert!(!app.accept_completion_on_enter());
    }

    #[test]
    fn parse_slash_dispatches_known_unknown_and_plain() {
        assert_eq!(parse_slash("hello"), SlashParse::NotCommand);
        assert_eq!(parse_slash("/help"), SlashParse::Known(SlashAction::Help));
        assert_eq!(parse_slash("/new"), SlashParse::Known(SlashAction::New));
        assert_eq!(parse_slash("/quit"), SlashParse::Known(SlashAction::Quit));
        assert_eq!(parse_slash("/exit"), SlashParse::Known(SlashAction::Quit));
        assert_eq!(
            parse_slash("/foobar"),
            SlashParse::Unknown("foobar".to_string())
        );
        // 首尾空白容错
        assert_eq!(parse_slash("  /new  "), SlashParse::Known(SlashAction::New));
    }

    #[test]
    fn system_item_and_clear_items() {
        let mut app = app();
        app.push_system(help_text());
        assert_eq!(app.items.len(), 1);
        let ChatItem::System(text) = &app.items[0] else {
            panic!("expected system item");
        };
        assert!(text.contains("/help"));
        assert!(text.contains("/new"));
        assert!(text.contains("/quit"));
        assert!(text.contains("/exit"));
        app.clear_items();
        assert!(app.items.is_empty());
    }

    #[test]
    fn dismiss_completion_reports_whether_popup_was_open() {
        let mut app = app();
        assert!(!app.dismiss_completion());
        app.insert_char('/');
        assert!(app.dismiss_completion());
        assert!(app.completion().is_none());
        // 关闭后下次编辑会重新计算
        app.insert_char('n');
        assert!(app.completion().is_some());
    }

    #[test]
    fn scroll_is_saturating() {
        let mut app = app();
        app.scroll_up(3);
        app.scroll_up(5);
        assert_eq!(app.scroll, 8);
        app.scroll_down(10);
        assert_eq!(app.scroll, 0);
        app.scroll_up(u16::MAX);
        app.scroll_up(1);
        assert_eq!(app.scroll, u16::MAX);
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
        assert_eq!(app.items.len(), 2);
        let ChatItem::User(text) = &app.items[0] else {
            panic!("expected user item");
        };
        assert_eq!(text, "问题");
        let ChatItem::Assistant(item) = &app.items[1] else {
            panic!("expected assistant item");
        };
        assert!(item.done);
        assert_eq!(item.blocks.len(), 2);
    }
}
