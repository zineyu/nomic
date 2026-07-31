//! TUI 状态层：聊天条目、流式增量累积、输入编辑、滚动。
//!
//! 本模块不碰终端，全部逻辑可脱离 ratatui/crossterm 单测。

use nomic_ai::{
    AssistantContent, AssistantEvent, Message, StopReason, UserContent, UserMessageContent,
};
use nomic_core::AgentEvent;
use nomic_skills::{ActivatedSkill, Skill};
use unicode_width::UnicodeWidthStr;

use crate::print::brief_args;

/// braille spinner 帧序列（运行中工具与流式指示共用）。
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
    /// 进度/结果的尾部摘要（最多 `DETAIL_LINES` 行）
    pub(super) detail: Vec<String>,
}

/// 一条 slash 命令的静态描述。
#[derive(Debug)]
pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) aliases: &'static [&'static str],
    pub(super) summary: &'static str,
    /// 参数形式非法时的用法提示
    pub(super) usage: &'static str,
}

/// 全部 slash 命令（补全候选与 `/help` 输出的唯一来源）。
pub(super) const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "help",
        aliases: &[],
        summary: "显示可用命令",
        usage: "/help",
    },
    SlashCommand {
        name: "new",
        aliases: &[],
        summary: "清空上下文，开启新对话（新 session）",
        usage: "/new",
    },
    SlashCommand {
        name: "resume",
        aliases: &[],
        summary: "选择并恢复历史 session（切换上下文与落库目标）",
        usage: "/resume",
    },
    SlashCommand {
        name: "compact",
        aliases: &[],
        summary: "压缩上下文为摘要（可带聚焦指令：/compact 专注某部分）",
        usage: "/compact [聚焦指令]",
    },
    SlashCommand {
        name: "skill",
        aliases: &[],
        summary: "手动载入 skill 到当前对话（/skill:<name>；无参列出可用 skill）",
        usage: "/skill:<name>（/skill 列出可用 skill）",
    },
    SlashCommand {
        name: "image",
        aliases: &[],
        summary: "为下一条消息附加图片（可多次附加；png/jpeg/gif/webp）",
        usage: "/image:<路径>（/image <路径> 亦可）",
    },
    SlashCommand {
        name: "quit",
        aliases: &["exit"],
        summary: "退出 TUI",
        usage: "/quit",
    },
];

/// slash 命令解析结果。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SlashParse {
    /// 输入不以 `/` 开头，按普通 prompt 处理
    NotCommand,
    /// 已知命令
    Known(SlashAction),
    /// 命令存在但参数形式非法（携带用法提示）
    InvalidUsage(&'static str),
    /// 未知命令名（不含 `/` 前缀）
    Unknown(String),
}

/// 已知 slash 命令的动作。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SlashAction {
    Help,
    New,
    Resume,
    Quit,
    /// `/skill`（None）列出可用 skill；`/skill:<name>` 载入指定 skill
    Skill(Option<String>),
    /// `/compact [聚焦指令]` 手动压缩上下文
    Compact(Option<String>),
    /// `/image <路径>` 为下一条消息附加图片
    Image(String),
}

/// 解析一行输入为 slash 命令。
///
/// 参数只支持 `/name:arg` 冒号形式（如 `/skill:jujutsu`）；
/// `/cmd extra` 视为参数形式非法。
pub(super) fn parse_slash(input: &str) -> SlashParse {
    let Some(rest) = input.trim().strip_prefix('/') else {
        return SlashParse::NotCommand;
    };
    // `/compact` 特判：参数是自由文本（可含空格），`/compact 指令` 与
    // `/compact:指令` 两种形式都接受
    if let Some(tail) = rest.strip_prefix("compact") {
        if tail.is_empty() {
            return SlashParse::Known(SlashAction::Compact(None));
        }
        if let Some(instructions) = tail.strip_prefix(':').or_else(|| tail.strip_prefix(' ')) {
            let instructions = instructions.trim();
            return SlashParse::Known(SlashAction::Compact(
                (!instructions.is_empty()).then(|| instructions.to_string()),
            ));
        }
        // `/compactxxx`：落入常规解析报未知命令
    }
    // `/image` 特判：参数是文件路径（可含空格），`/image 路径` 与
    // `/image:路径` 两种形式都接受
    if let Some(tail) = rest.strip_prefix("image") {
        if let Some(path) = tail.strip_prefix(':').or_else(|| tail.strip_prefix(' ')) {
            let path = path.trim();
            return if path.is_empty() {
                SlashParse::InvalidUsage(image_usage())
            } else {
                SlashParse::Known(SlashAction::Image(path.to_string()))
            };
        }
        if tail.is_empty() {
            return SlashParse::InvalidUsage(image_usage());
        }
        // `/imagexxx`：落入常规解析报未知命令
    }
    let (name, arg, junk) = if let Some((name, arg)) = rest.split_once(':') {
        (
            name.trim(),
            Some(arg.trim()).filter(|arg| !arg.is_empty()),
            false,
        )
    } else {
        let mut parts = rest.split_whitespace();
        let name = parts.next().unwrap_or_default();
        let junk = parts.next().is_some();
        (name, None, junk)
    };
    for command in SLASH_COMMANDS {
        if command.name == name || command.aliases.contains(&name) {
            let action = match command.name {
                "skill" => {
                    if junk || arg.is_some_and(|arg| arg.contains(char::is_whitespace)) {
                        return SlashParse::InvalidUsage(command.usage);
                    }
                    SlashAction::Skill(arg.map(str::to_string))
                }
                "help" if !junk && arg.is_none() => SlashAction::Help,
                "new" if !junk && arg.is_none() => SlashAction::New,
                "resume" if !junk && arg.is_none() => SlashAction::Resume,
                "quit" if !junk && arg.is_none() => SlashAction::Quit,
                _ => return SlashParse::InvalidUsage(command.usage),
            };
            return SlashParse::Known(action);
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

/// 补全候选：slash 命令或 `/skill:` 后的 skill 名。
#[derive(Debug)]
pub(super) enum CompletionCandidate {
    Command(&'static SlashCommand),
    Skill(SkillEntry),
}

impl CompletionCandidate {
    /// 候选对应的输入片段（不含 `/` 前缀），用于精确匹配、排序与填入。
    fn fragment(&self) -> String {
        match self {
            Self::Command(command) => command.name.to_string(),
            Self::Skill(entry) => format!("skill:{}", entry.name),
        }
    }

    /// 输入片段是否精确对应该候选（Enter 是否可直接提交）。
    fn matches_fragment(&self, fragment: &str) -> bool {
        match self {
            Self::Command(command) => {
                command.name == fragment || command.aliases.contains(&fragment)
            }
            Self::Skill(_) => self.fragment() == fragment,
        }
    }
}

/// 可用于 `/skill:` 补全的 skill 元数据（启动时从 resolver catalog 快照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkillEntry {
    pub(super) name: String,
    pub(super) description: String,
}

/// 补全弹层状态：候选列表 + 当前选中项。
#[derive(Debug)]
pub(super) struct Completion {
    pub(super) candidates: Vec<CompletionCandidate>,
    pub(super) selected: usize,
}

/// `/resume` 选择器的一行：session id + 预生成的展示文本（渲染零计算）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResumeRow {
    pub(super) id: String,
    pub(super) text: String,
}

/// `/resume` session 选择器状态：候选行 + 当前选中项（移动到底/顶钳制，不循环）。
#[derive(Debug)]
pub(super) struct ResumePicker {
    pub(super) rows: Vec<ResumeRow>,
    pub(super) selected: usize,
}

/// 暂存的图片附件（`/image <路径>` 载入，随下一条 prompt 一起发送）。
#[derive(Debug)]
pub(super) struct PendingImage {
    /// 展示名（文件名）
    pub(super) name: String,
    /// 图片内容块（base64 内联）
    pub(super) image: nomic_ai::ImageContent,
}

/// TUI 应用状态。
#[derive(Debug)]
pub(super) struct App {
    pub(super) items: Vec<ChatItem>,
    /// 输入缓冲（可多行，`\n` 为 Shift+Enter 插入的手动换行）
    input: String,
    /// 光标位置（字节索引，始终落在 char 边界）
    cursor: usize,
    /// slash 命令补全弹层（输入以 `/` 开头时出现）
    completion: Option<Completion>,
    /// `/resume` session 选择器（打开时接管键位）
    resume_picker: Option<ResumePicker>,
    /// 暂存的图片附件（随下一条 prompt 发送）
    pub(super) attachments: Vec<PendingImage>,
    /// 从底部向上滚动的行数（0 = 跟随最新内容）
    pub(super) scroll: u16,
    /// 聊天区最大可上滚行数（渲染时更新，状态栏滚动位置显示用）
    pub(super) scroll_max: u16,
    pub(super) running: bool,
    pub(super) should_quit: bool,
    /// 模型展示名
    pub(super) model_name: String,
    /// 当前 session id（未持久化时为 None）
    pub(super) session_id: Option<String>,
    /// 状态栏一次性提示（告警等）
    pub(super) notice: Option<String>,
    /// spinner 帧序号（仅运行中由事件循环周期推进）
    spinner: usize,
    /// `/skill:` 补全用的可用 skill 快照
    skills: Vec<SkillEntry>,
}

impl App {
    pub(super) const fn new(model_name: String, session_id: Option<String>) -> Self {
        Self {
            items: Vec::new(),
            input: String::new(),
            cursor: 0,
            completion: None,
            resume_picker: None,
            attachments: Vec::new(),
            scroll: 0,
            scroll_max: 0,
            running: false,
            should_quit: false,
            model_name,
            session_id,
            notice: None,
            spinner: 0,
            skills: Vec::new(),
        }
    }

    /// 设置 `/skill:` 补全用的可用 skill 快照（启动时从 resolver catalog 取）。
    pub(super) fn set_available_skills(&mut self, skills: Vec<SkillEntry>) {
        self.skills = skills;
    }

    /// 把 resume 恢复的历史消息渲染为聊天条目。
    pub(super) fn load_history(&mut self, messages: &[Message]) {
        for message in messages {
            match message {
                Message::User(user) => self.push_user_text(user_text(&user.content)),
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
                    self.push_user_text(user_text(&user.content));
                }
                Message::Assistant(_) => {
                    self.items
                        .push(ChatItem::Assistant(AssistantItem::default()));
                }
                Message::ToolResult(_) => {}
            },
            AgentEvent::MessageUpdate(delta) => self.apply_delta(delta),
            AgentEvent::MessageEnd(message) => {
                if let Message::Assistant(assistant) = message.as_ref()
                    && let Some(ChatItem::Assistant(item)) = self.items.last_mut()
                {
                    item.done = true;
                    item.error =
                        assistant_error(assistant.stop_reason, assistant.error_message.as_deref());
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
                    args: brief_args(tool_name, args),
                    status: ToolStatus::Running,
                    detail: Vec::new(),
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
            AgentEvent::CompactionStart { tokens_before } => {
                // 用一次性提示而非聊天条目：压缩失败时提示自然消失，不残留
                self.notice = Some(format!("正在压缩上下文（约 {tokens_before} tokens）…"));
            }
            AgentEvent::CompactionEnd {
                tokens_before,
                kept_count,
                ..
            } => {
                self.notice = None;
                self.push_system(format!(
                    "上下文已压缩：约 {tokens_before} tokens → 摘要 + {kept_count} 条近期消息。"
                ));
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

    /// 光标位置（逻辑行号, 行内显示宽度）：多行输入框渲染光标用。
    pub(super) fn cursor_position(&self) -> (u16, u16) {
        let before = &self.input[..self.cursor];
        let row = before.bytes().filter(|b| *b == b'\n').count();
        let col = before.rsplit('\n').next().map_or(0, UnicodeWidthStr::width);
        (
            u16::try_from(row).unwrap_or(u16::MAX),
            u16::try_from(col).unwrap_or(u16::MAX),
        )
    }

    /// 输入的逻辑行数（空输入为 1），输入框高度据此伸缩。
    pub(super) fn line_count(&self) -> u16 {
        let count = self.input.bytes().filter(|b| *b == b'\n').count() + 1;
        u16::try_from(count).unwrap_or(u16::MAX)
    }

    pub(super) fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.refresh_completion();
    }

    /// Shift+Enter 手动换行：换行是空白字符，补全弹层随之关闭。
    pub(super) fn insert_newline(&mut self) {
        self.insert_char('\n');
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

    /// 暂存一张图片附件，返回当前附件总数。
    pub(super) fn stage_image(&mut self, name: String, image: nomic_ai::ImageContent) -> usize {
        self.attachments.push(PendingImage { name, image });
        self.attachments.len()
    }

    /// 是否有暂存的图片附件。
    pub(super) const fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    /// 取出全部暂存附件（prompt 提交时随文本一起带走）。
    pub(super) fn take_attachments(&mut self) -> Vec<nomic_ai::ImageContent> {
        self.attachments
            .drain(..)
            .map(|pending| pending.image)
            .collect()
    }

    // ── slash 命令补全 ──────────────────────────────────────────────────────

    /// 当前补全弹层（渲染用）。
    pub(super) const fn completion(&self) -> Option<&Completion> {
        self.completion.as_ref()
    }

    /// 按当前输入重算补全候选：仅在「以 `/` 开头、光标在末尾、命令名
    /// 未输入完整参数（无空白）」时弹出；`/skill:` 后切换为 skill 名候选。
    fn refresh_completion(&mut self) {
        let Some(fragment) = self.slash_fragment().map(str::to_string) else {
            self.completion = None;
            return;
        };
        self.completion = if let Some(name_fragment) = fragment.strip_prefix("skill:") {
            self.skill_candidates(name_fragment)
        } else {
            Self::command_candidates(&fragment)
        };
    }

    /// slash 命令候选（按命令名/别名前缀匹配，按名称排序）。
    fn command_candidates(fragment: &str) -> Option<Completion> {
        let mut candidates: Vec<CompletionCandidate> = SLASH_COMMANDS
            .iter()
            .filter(|command| {
                command.name.starts_with(fragment)
                    || command.aliases.iter().any(|a| a.starts_with(fragment))
            })
            .map(CompletionCandidate::Command)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(CompletionCandidate::fragment);
        // 输入已精确匹配某命令时选中它，Tab 从它开始循环
        let selected = candidates
            .iter()
            .position(|candidate| candidate.fragment() == fragment)
            .unwrap_or(0);
        Some(Completion {
            candidates,
            selected,
        })
    }

    /// `/skill:` 后的 skill 名候选（按名称前缀匹配）。
    fn skill_candidates(&self, name_fragment: &str) -> Option<Completion> {
        let mut candidates: Vec<CompletionCandidate> = self
            .skills
            .iter()
            .filter(|entry| entry.name.starts_with(name_fragment))
            .map(|entry| CompletionCandidate::Skill(entry.clone()))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(CompletionCandidate::fragment);
        let selected = candidates
            .iter()
            .position(|candidate| candidate.fragment() == format!("skill:{name_fragment}"))
            .unwrap_or(0);
        Some(Completion {
            candidates,
            selected,
        })
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
        let current = completion.candidates[completion.selected].fragment();
        let selected = if self.input == format!("/{current}") {
            (completion.selected + 1) % completion.candidates.len()
        } else {
            completion.selected
        };
        let fragment = completion.candidates[selected].fragment();
        self.input = format!("/{fragment}");
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

    /// Enter 且补全弹层可见时的智能接受：输入未精确匹配任何候选时
    /// 填入选中候选（返回 `true`，不提交）；已精确匹配则返回 `false` 正常提交。
    pub(super) fn accept_completion_on_enter(&mut self) -> bool {
        let Some(fragment) = self.slash_fragment() else {
            return false;
        };
        let Some(completion) = &self.completion else {
            return false;
        };
        let exact = completion
            .candidates
            .iter()
            .any(|candidate| candidate.matches_fragment(fragment));
        if exact {
            return false;
        }
        self.tab_complete();
        true
    }

    // ── /resume session 选择器 ──────────────────────────────────────────────

    /// 打开 `/resume` 选择器；调用方保证候选非空。
    pub(super) fn open_resume_picker(&mut self, rows: Vec<ResumeRow>) {
        debug_assert!(!rows.is_empty());
        self.resume_picker = Some(ResumePicker { rows, selected: 0 });
    }

    /// 当前选择器（渲染与键位路由用）。
    pub(super) const fn resume_picker(&self) -> Option<&ResumePicker> {
        self.resume_picker.as_ref()
    }

    /// 关闭选择器（Esc/q 取消）。
    pub(super) fn close_resume_picker(&mut self) {
        self.resume_picker = None;
    }

    /// 移动选中项（到底/顶钳制，不循环）。
    pub(super) fn resume_select(&mut self, delta: isize) {
        if let Some(picker) = &mut self.resume_picker {
            let last = picker.rows.len() - 1;
            picker.selected = picker.selected.saturating_add_signed(delta).min(last);
        }
    }

    /// Enter 确认：取出选中 session id 并关闭选择器。
    pub(super) fn take_resume_selection(&mut self) -> Option<String> {
        let picker = self.resume_picker.take()?;
        Some(picker.rows[picker.selected].id.clone())
    }

    // ── slash 命令反馈 ──────────────────────────────────────────────────────

    /// 追加一条 user 聊天条目；skill 注入消息与压缩摘要消息压缩为系统提示样式的一行。
    fn push_user_text(&mut self, text: String) {
        if let Some(notice) = skill_load_notice(&text) {
            self.items.push(ChatItem::System(notice));
        } else if text.starts_with(nomic_ai::SUMMARY_PREFIX) {
            self.items.push(ChatItem::System(
                "更早的对话已压缩为摘要注入上下文。".to_string(),
            ));
        } else {
            self.items.push(ChatItem::User(text));
        }
        self.scroll_to_bottom();
    }

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

    // ── spinner ─────────────────────────────────────────────────────────────

    /// 推进 spinner 一帧（事件循环在运行中周期调用）。
    pub(super) const fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    /// 当前 spinner 帧字符。
    pub(super) const fn spinner(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner % SPINNER_FRAMES.len()]
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

/// `/image` 的用法提示（以 SLASH_COMMANDS 为唯一来源）。
fn image_usage() -> &'static str {
    SLASH_COMMANDS
        .iter()
        .find(|command| command.name == "image")
        .map_or("/image:<路径>", |command| command.usage)
}

fn user_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(text) => text.clone(),
        UserMessageContent::Blocks(blocks) => {
            let text = blocks_text(blocks);
            let images = blocks
                .iter()
                .filter(|block| matches!(block, UserContent::Image(_)))
                .count();
            if images == 0 {
                text
            } else {
                // 图片块无法内联渲染，以占位行标示（与块序一致：图片在前）
                format!("🖼 图片 ×{images}\n{text}")
            }
        }
    }
}

// ── skill 手动载入（`/skill:<name>`）────────────────────────────────────────

/// 构造手动载入 skill 的注入文本（作为 user 消息进入上下文，随 session 落库）。
///
/// 标签格式与 bootstrap 中 `--skill` 注入 system prompt 的 `<active_skill>` 一致，
/// 模型侧无需区分来源。
pub(super) fn skill_load_message(skill: &ActivatedSkill) -> String {
    format!(
        "<active_skill name=\"{}\" scope=\"{}\" path=\"{}\">\n{}\n</active_skill>\n\n\
         The user manually loaded this skill into the conversation. \
         Follow its instructions for the subsequent work.",
        skill.name,
        skill.scope,
        skill.path.display(),
        skill.instructions
    )
}

/// `/skill` 无参时展示的可用 skill 清单（本地展示，不进上下文）。
pub(super) fn skill_list_text(skills: &[Skill]) -> String {
    use std::fmt::Write as _;
    if skills.is_empty() {
        return "没有可用的 skill（查找 .nomic/skills、.agents/skills 与用户配置目录）。"
            .to_string();
    }
    let mut text = "可用 skill（/skill:<name> 载入）：".to_string();
    for skill in skills {
        let _ = write!(
            text,
            "\n  {} — {}（{}）",
            skill.name, skill.document.description, skill.scope
        );
    }
    text
}

/// 聊天区压缩展示注入的 skill 消息：返回 `Some` 表示该 user 文本是 skill 注入。
fn skill_load_notice(text: &str) -> Option<String> {
    let header = text.strip_prefix("<active_skill ")?;
    let name = xml_attr(header, "name")?;
    Some(match xml_attr(header, "path") {
        Some(path) => format!("已载入 skill `{name}`（{path}）"),
        None => format!("已载入 skill `{name}`"),
    })
}

/// 从 `<active_skill ...>` 头中提取属性值（仅用于展示，解析失败回退完整文本）。
fn xml_attr(header: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = header.find(&needle)? + needle.len();
    let end = header[start..].find('"')? + start;
    Some(header[start..end].to_string())
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

/// 工具结果摘要的最大行数（聊天区保持紧凑，只留尾部上下文）。
const DETAIL_LINES: usize = 3;

/// 提取工具输出的尾部摘要：非空行 trim 后取最后 `DETAIL_LINES` 行，
/// 每行截断到 120 字符（超长由渲染层折行兜底，这里先压住极端长行）。
fn result_summary(blocks: &[UserContent]) -> Vec<String> {
    const MAX_LINE: usize = 120;
    let text = blocks_text(blocks);
    let mut tail: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    tail = tail.split_off(tail.len().saturating_sub(DETAIL_LINES));
    tail.into_iter()
        .map(|line| {
            if line.chars().count() <= MAX_LINE {
                line.to_string()
            } else {
                let truncated: String = line.chars().take(MAX_LINE).collect();
                format!("{truncated}…")
            }
        })
        .collect()
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
    use std::path::PathBuf;

    use nomic_ai::{ApiKind, AssistantMessage, TextContent, ThinkingContent, Usage, UserMessage};
    use nomic_core::{ToolResult, ToolUpdate};
    use nomic_skills::{SkillDocument, SkillScope};

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

        let [ChatItem::Tool(first), ChatItem::Tool(second)] = &app.items[..] else {
            panic!("unexpected items");
        };
        assert_eq!(first.status, ToolStatus::Failed);
        assert_eq!(second.status, ToolStatus::Running);
    }

    #[test]
    fn multiline_input_tracks_lines_and_cursor() {
        let mut app = app();
        assert_eq!(app.line_count(), 1);
        assert_eq!(app.cursor_position(), (0, 0));

        for c in "你好".chars() {
            app.insert_char(c);
        }
        app.insert_newline();
        for c in "ab".chars() {
            app.insert_char(c);
        }
        assert_eq!(app.input(), "你好\nab");
        assert_eq!(app.line_count(), 2);
        // 光标在第二行末尾：行号 1，行内宽度 2
        assert_eq!(app.cursor_position(), (1, 2));

        // 光标移回第一行行尾（CJK 宽度 4）
        app.cursor_left();
        app.cursor_left();
        app.cursor_left();
        assert_eq!(app.cursor_position(), (0, 4));

        // 多行输入可整体提交
        assert_eq!(app.take_input().as_deref(), Some("你好\nab"));
        assert_eq!(app.line_count(), 1);
    }

    #[test]
    fn newline_dismisses_completion() {
        let mut app = app();
        app.insert_char('/');
        assert!(app.completion().is_some());
        // 换行是空白字符，slash 补全随之关闭
        app.insert_newline();
        assert!(app.completion().is_none());
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
        assert_eq!(candidate_fragments(completion), vec!["new"]);

        // Tab 接受候选
        app.tab_complete();
        assert_eq!(app.input(), "/new");
        // 精确匹配后仍显示（展示描述），且选中该项
        let completion = app.completion().expect("精确匹配仍显示候选");
        assert_eq!(completion.candidates[completion.selected].fragment(), "new");

        // 输入空格（进入参数区）后弹层消失
        app.insert_char(' ');
        assert!(app.completion().is_none());
    }

    /// 候选的输入片段列表（不含 `/` 前缀），测试断言用。
    fn candidate_fragments(completion: &Completion) -> Vec<String> {
        completion
            .candidates
            .iter()
            .map(CompletionCandidate::fragment)
            .collect()
    }

    #[test]
    fn slash_completion_matches_alias_and_enter_accepts() {
        let mut app = app();
        for c in "/ex".chars() {
            app.insert_char(c);
        }
        let completion = app.completion().expect("/ex 匹配别名 exit");
        assert_eq!(
            completion.candidates[completion.selected].fragment(),
            "quit"
        );

        // 未精确匹配时 Enter 先填入候选，不提交
        assert!(app.accept_completion_on_enter());
        assert_eq!(app.input(), "/quit");
        // 精确匹配后 Enter 放行提交
        assert!(!app.accept_completion_on_enter());
    }

    #[test]
    fn resume_picker_clamps_selection_and_take_closes() {
        let mut app = app();
        let rows = (0..3)
            .map(|i| ResumeRow {
                id: format!("id-{i}"),
                text: format!("row {i}"),
            })
            .collect();
        app.open_resume_picker(rows);

        // 到底/顶钳制，不循环
        app.resume_select(1);
        app.resume_select(1);
        app.resume_select(1);
        assert_eq!(app.resume_picker().expect("picker").selected, 2);
        app.resume_select(-5);
        assert_eq!(app.resume_picker().expect("picker").selected, 0);

        // Enter 确认：返回选中 id 并关闭；关闭后再次确认为 None
        app.resume_select(1);
        assert_eq!(app.take_resume_selection().as_deref(), Some("id-1"));
        assert!(app.resume_picker().is_none());
        assert!(app.take_resume_selection().is_none());
    }

    #[test]
    fn parse_slash_dispatches_known_unknown_and_plain() {
        assert_eq!(parse_slash("hello"), SlashParse::NotCommand);
        assert_eq!(parse_slash("/help"), SlashParse::Known(SlashAction::Help));
        assert_eq!(parse_slash("/new"), SlashParse::Known(SlashAction::New));
        assert_eq!(
            parse_slash("/resume"),
            SlashParse::Known(SlashAction::Resume)
        );
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
    fn parse_slash_skill_uses_colon_argument() {
        assert_eq!(
            parse_slash("/skill"),
            SlashParse::Known(SlashAction::Skill(None))
        );
        assert_eq!(
            parse_slash("/skill:jujutsu"),
            SlashParse::Known(SlashAction::Skill(Some("jujutsu".to_string())))
        );
        // 空参数等价于无参（列出清单）
        assert_eq!(
            parse_slash("/skill:"),
            SlashParse::Known(SlashAction::Skill(None))
        );
        // 空白分隔的参数与带空格的参数均属于非法用法
        assert!(matches!(
            parse_slash("/skill jujutsu"),
            SlashParse::InvalidUsage(_)
        ));
        assert!(matches!(
            parse_slash("/skill:a b"),
            SlashParse::InvalidUsage(_)
        ));
        // 无参命令带参数同样报用法错误
        assert!(matches!(parse_slash("/new x"), SlashParse::InvalidUsage(_)));
        assert!(matches!(
            parse_slash("/resume:abc"),
            SlashParse::InvalidUsage(_)
        ));
        assert!(matches!(
            parse_slash("/quit:now"),
            SlashParse::InvalidUsage(_)
        ));
        // 未知命令带冒号参数仍报未知
        assert_eq!(
            parse_slash("/foo:bar"),
            SlashParse::Unknown("foo".to_string())
        );
    }

    #[test]
    fn parse_slash_compact_takes_free_text_instructions() {
        assert_eq!(
            parse_slash("/compact"),
            SlashParse::Known(SlashAction::Compact(None))
        );
        // 空白分隔的自由文本（可含空格）
        assert_eq!(
            parse_slash("/compact 专注 测试 部分"),
            SlashParse::Known(SlashAction::Compact(Some("专注 测试 部分".to_string())))
        );
        // 冒号形式同样接受
        assert_eq!(
            parse_slash("/compact:focus on tests"),
            SlashParse::Known(SlashAction::Compact(Some("focus on tests".to_string())))
        );
        // 空参数等价于无参
        assert_eq!(
            parse_slash("/compact "),
            SlashParse::Known(SlashAction::Compact(None))
        );
        // 前缀不等于命令名：/compactx 报未知
        assert_eq!(
            parse_slash("/compactx"),
            SlashParse::Unknown("compactx".to_string())
        );
    }

    #[test]
    fn parse_slash_image_takes_path_argument() {
        assert_eq!(
            parse_slash("/image:pic.png"),
            SlashParse::Known(SlashAction::Image("pic.png".to_string()))
        );
        // 空白分隔形式同样接受（路径可含空格）
        assert_eq!(
            parse_slash("/image my pics/a.png"),
            SlashParse::Known(SlashAction::Image("my pics/a.png".to_string()))
        );
        // 无参数报用法
        assert!(matches!(parse_slash("/image"), SlashParse::InvalidUsage(_)));
        assert!(matches!(
            parse_slash("/image "),
            SlashParse::InvalidUsage(_)
        ));
        // 前缀不等于命令名：/imagex 报未知
        assert_eq!(
            parse_slash("/imagex"),
            SlashParse::Unknown("imagex".to_string())
        );
    }

    #[test]
    fn staged_attachments_follow_next_prompt() {
        let mut app = app();
        let image = || nomic_ai::ImageContent {
            data: "aA==".to_string(),
            mime_type: "image/png".to_string(),
        };
        assert!(!app.has_attachments());
        assert_eq!(app.stage_image("a.png".to_string(), image()), 1);
        assert_eq!(app.stage_image("b.png".to_string(), image()), 2);
        assert!(app.has_attachments());
        let taken = app.take_attachments();
        assert_eq!(taken.len(), 2);
        assert!(!app.has_attachments());
        // 取空后再次取出为空
        assert!(app.take_attachments().is_empty());
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

    #[test]
    fn compaction_events_render_as_system_lines() {
        let mut app = app();
        app.handle_event(&AgentEvent::CompactionStart {
            tokens_before: 150_000,
        });
        // 压缩中只置状态栏提示，不进聊天区（失败时不残留）
        assert!(app.items.is_empty());
        assert!(app.notice.as_deref().is_some_and(|n| n.contains("压缩")));
        app.handle_event(&AgentEvent::CompactionEnd {
            summary: "## Goal\nwork".to_string(),
            tokens_before: 150_000,
            kept_count: 7,
            usage: Usage::default(),
        });
        assert!(app.notice.is_none());
        let system_lines: Vec<&str> = app
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
        assert!(matches!(&app.items[0], ChatItem::System(text) if text.contains("已压缩")));
        assert!(matches!(&app.items[1], ChatItem::User(text) if text == "recent question"));
    }

    #[test]
    fn skill_completion_after_colon_prefix() {
        let mut app = app();
        app.set_available_skills(vec![
            SkillEntry {
                name: "jujutsu".to_string(),
                description: "jj vcs".to_string(),
            },
            SkillEntry {
                name: "rust-review".to_string(),
                description: "review rust".to_string(),
            },
        ]);
        for c in "/skill:".chars() {
            app.insert_char(c);
        }
        let completion = app.completion().expect("/skill: 弹出全部 skill");
        assert_eq!(
            candidate_fragments(completion),
            vec!["skill:jujutsu", "skill:rust-review"]
        );

        // Tab 接受选中项；接受后候选收敛到精确匹配项，再次 Tab 保持不变
        app.tab_complete();
        assert_eq!(app.input(), "/skill:jujutsu");
        app.tab_complete();
        assert_eq!(app.input(), "/skill:jujutsu");

        // 前缀过滤后 Enter 填入唯一候选，再次 Enter 精确匹配放行提交
        app.take_input();
        for c in "/skill:juj".chars() {
            app.insert_char(c);
        }
        let completion = app.completion().expect("前缀过滤");
        assert_eq!(candidate_fragments(completion), vec!["skill:jujutsu"]);
        assert!(app.accept_completion_on_enter());
        assert_eq!(app.input(), "/skill:jujutsu");
        assert!(!app.accept_completion_on_enter());
    }

    #[test]
    fn skill_load_message_renders_compactly_in_chat_and_history() {
        let skill = ActivatedSkill {
            name: "jujutsu".to_string(),
            scope: SkillScope::Project,
            path: PathBuf::from("/repo/.agents/skills/jujutsu/SKILL.md"),
            instructions: "do jj things".to_string(),
        };
        let message = skill_load_message(&skill);
        assert!(
            message.starts_with(
                "<active_skill name=\"jujutsu\" scope=\"project\" \
                 path=\"/repo/.agents/skills/jujutsu/SKILL.md\">"
            ),
            "{message}"
        );
        assert!(message.contains("do jj things"));
        assert!(message.contains("manually loaded"));

        // 运行中注入：聊天区压缩为一行系统样式提示
        let mut chat = app();
        chat.handle_event(&AgentEvent::MessageStart(user_message(&message)));
        assert_eq!(chat.items.len(), 1);
        let ChatItem::System(text) = &chat.items[0] else {
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
        assert!(matches!(resumed.items[0], ChatItem::System(_)));

        // 普通 user 消息不受影响
        let mut plain = app();
        plain.handle_event(&AgentEvent::MessageStart(user_message("普通问题")));
        assert!(matches!(plain.items[0], ChatItem::User(_)));
    }

    #[test]
    fn skill_list_text_lists_names_or_reports_empty() {
        assert!(skill_list_text(&[]).contains("没有可用的 skill"));
        let skill = Skill {
            name: "jujutsu".to_string(),
            path: PathBuf::from("/repo/.agents/skills/jujutsu/SKILL.md"),
            root: PathBuf::from("/repo/.agents/skills/jujutsu"),
            scope: SkillScope::Project,
            document: SkillDocument {
                description: "jj vcs".to_string(),
                triggers: Vec::new(),
                body: "body".to_string(),
            },
        };
        let text = skill_list_text(&[skill]);
        assert!(text.contains("/skill:<name>"), "{text}");
        assert!(text.contains("jujutsu — jj vcs（project）"), "{text}");
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
        assert!(text.contains("/skill"));
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
    fn tick_advances_spinner_frame() {
        let mut app = app();
        let first = app.spinner();
        app.tick();
        assert_ne!(app.spinner(), first);
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
