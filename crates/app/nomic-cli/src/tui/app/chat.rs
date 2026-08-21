//! 聊天区状态：条目列表、流式增量累积与滚动。
//!
//! 条目模型（user/assistant/tool/system）与 [`Chat`] 状态结构自持；
//! 复制等操作返回数据，提示语由模式路由层（[`super::App`]）落到状态栏。

use nomic_ai::{
    AssistantContent, AssistantEvent, Message, StopReason, UserContent, UserMessageContent,
};
use nomic_skills::{ActivatedSkill, parse_active_skill_tag};

use crate::mention;
use crate::tui::chat_lines::chat_lines;

/// 聊天区条目。
#[derive(Debug)]
pub(in crate::tui) enum ChatItem {
    /// 用户消息
    User(String),
    /// assistant 消息（流式中逐步累积）
    Assistant(AssistantItem),
    /// 一次工具执行
    Tool(ToolItem),
    /// 本地系统提示（命令输出等，不进上下文）
    System(String),
}

impl ChatItem {
    /// 是否为对话消息（user/assistant）：`copy` 命令与 NORMAL `Y` 的复制目标。
    pub(super) const fn is_message(&self) -> bool {
        matches!(self, Self::User(_) | Self::Assistant(_))
    }
}

/// assistant 消息条目：有序内容块 + 定稿状态。
#[derive(Debug, Default)]
pub(in crate::tui) struct AssistantItem {
    pub(in crate::tui) blocks: Vec<Block>,
    pub(in crate::tui) done: bool,
    /// `stop_reason` 为 Error/Aborted 时的错误信息
    pub(in crate::tui) error: Option<String>,
}

/// assistant 内容块（工具调用块不进聊天区，由 `ToolExecution*` 事件承载）。
#[derive(Debug)]
pub(in crate::tui) enum Block {
    Text(String),
    Thinking(String),
}

/// 工具执行状态。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tui) enum ToolStatus {
    Running,
    Ok,
    Failed,
}

/// 工具执行条目。
#[derive(Debug)]
pub(in crate::tui) struct ToolItem {
    /// 工具调用 id（并行执行时按 id 匹配 update/end）
    pub(in crate::tui) id: String,
    pub(in crate::tui) name: String,
    /// 参数摘要（截断）
    pub(in crate::tui) args: String,
    pub(in crate::tui) status: ToolStatus,
    /// 进度/结果的尾部摘要（最多 `DETAIL_LINES` 行）
    pub(in crate::tui) detail: Vec<String>,
}

/// 聊天区状态：条目 + 滚动。不碰交互模式，由 [`super::App`]
/// 组合并按模式路由调用。
#[derive(Debug, Default)]
pub(in crate::tui) struct Chat {
    pub(super) items: Vec<ChatItem>,
    /// 从底部向上滚动的行数（0 = 跟随最新内容）
    pub(super) scroll: u16,
    /// 聊天区最大可上滚行数（[`Self::sync_geometry`] 按视口计算，状态栏滚动位置显示用）
    scroll_max: u16,
}

impl Chat {
    // ── 条目 ────────────────────────────────────────────────────────────────

    /// 聊天区条目（渲染用）。
    pub(in crate::tui) fn items(&self) -> &[ChatItem] {
        &self.items
    }

    /// 追加一条 user 聊天条目；skill 注入消息与压缩摘要消息压缩为系统提示样式的一行。
    pub(super) fn push_user_text(&mut self, text: &str) {
        if let Some(notice) = skill_load_notice(text) {
            self.items.push(ChatItem::System(notice));
        } else if text.starts_with(nomic_ai::SUMMARY_PREFIX) {
            self.items.push(ChatItem::System(
                "更早的对话已压缩为摘要注入上下文。".to_string(),
            ));
        } else {
            self.items
                .push(ChatItem::User(collapse_mention_blocks(text)));
        }
        self.scroll_to_bottom();
    }

    /// 追加一条本地系统提示（不进上下文、不落库）。
    pub(in crate::tui) fn push_system(&mut self, text: impl Into<String>) {
        self.items.push(ChatItem::System(text.into()));
        self.scroll_to_bottom();
    }

    /// 清空聊天区（`new` 开启新对话、`resume` 恢复前）。
    pub(super) fn clear_items(&mut self) {
        self.items.clear();
        self.scroll_to_bottom();
    }

    /// 把 resume 恢复的历史消息渲染为聊天条目。
    pub(super) fn load_history(&mut self, messages: &[Message]) {
        for message in messages {
            match message {
                Message::User(user) => self.push_user_text(&user_text(&user.content)),
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

    /// 开始一条流式 assistant 消息（`MessageStart`）。
    pub(super) fn start_assistant(&mut self) {
        self.items
            .push(ChatItem::Assistant(AssistantItem::default()));
    }

    /// 定稿最新一条 assistant 消息（`MessageEnd`）：携带
    /// `stop_reason` 为 Error/Aborted 时的错误信息。
    pub(super) fn finalize_assistant(&mut self, error: Option<String>) {
        if let Some(ChatItem::Assistant(item)) = self.items.last_mut() {
            item.done = true;
            item.error = error;
        }
    }

    /// 按 `(index, delta)` 累积流式 assistant 内容（ADR-0001 消费方义务）。
    pub(super) fn apply_delta(&mut self, delta: &AssistantEvent) {
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

    /// 追加一次工具执行（`ToolExecutionStart`），并滚到底跟随输出。
    pub(super) fn push_tool(&mut self, tool: ToolItem) {
        self.items.push(ChatItem::Tool(tool));
        self.scroll_to_bottom();
    }

    /// 更新工具执行的进度摘要（`ToolExecutionUpdate`）。
    pub(super) fn update_tool_detail(&mut self, tool_call_id: &str, content: &[UserContent]) {
        if let Some(tool) = self.find_tool_mut(tool_call_id) {
            tool.detail = result_summary(content);
        }
    }

    /// 定稿工具执行（`ToolExecutionEnd`）：状态 + 结果摘要。
    pub(super) fn finish_tool(
        &mut self,
        tool_call_id: &str,
        is_error: bool,
        content: &[UserContent],
    ) {
        if let Some(tool) = self.find_tool_mut(tool_call_id) {
            tool.status = if is_error {
                ToolStatus::Failed
            } else {
                ToolStatus::Ok
            };
            tool.detail = result_summary(content);
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

    /// `continue` 命令：弹出聊天区尾部失败/未定稿的 assistant 条目（随历史中的
    /// 失败消息一并移除，与 `Agent::continue_run` 同一口径）。
    pub(super) fn pop_trailing_failed_assistant(&mut self) {
        while matches!(
            self.items.last(),
            Some(ChatItem::Assistant(item)) if item.error.is_some() || !item.done
        ) {
            self.items.pop();
        }
    }

    /// `copy` 命令的复制源：聊天区最新一条用户/assistant 消息的纯文本
    ///（[`item_text`] 口径）；全部为空返回 `None`。
    pub(super) fn latest_message_text(&self) -> Option<String> {
        self.items
            .iter()
            .rev()
            .filter(|item| item.is_message())
            .find_map(item_text)
    }

    // ── 滚动 ────────────────────────────────────────────────────────────────

    pub(in crate::tui) const fn scroll_up(&mut self, lines: u16) {
        self.scroll = self.scroll.saturating_add(lines);
    }

    pub(in crate::tui) const fn scroll_down(&mut self, lines: u16) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub(super) const fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }

    /// 渲染前按已知视口计算聊天区几何：以与上屏相同的行组装
    ///（[`crate::tui::chat_lines`]）折行，回写滚动上限并就地钳制滚动
    /// 偏移。几何由此进状态层（每帧渲染前由 [`super::App::sync_chat_geometry`]
    /// 调用一次），渲染 widget 只读；按键行为与测试不再依赖「上一帧是否画过」。
    pub(in crate::tui) fn sync_geometry(
        &mut self,
        width: u16,
        height: u16,
        thinking_collapsed: bool,
        spinner: &str,
    ) {
        let lines = chat_lines(&self.items, width, thinking_collapsed, spinner);
        self.scroll_max =
            u16::try_from(lines.len().saturating_sub(usize::from(height))).unwrap_or(u16::MAX);
        self.scroll = self.scroll.min(self.scroll_max);
    }

    /// 当前滚动偏移（从底部向上计）。
    pub(in crate::tui) const fn scroll(&self) -> u16 {
        self.scroll
    }

    /// 聊天区最大可上滚行数（几何同步后有效）。
    pub(in crate::tui) const fn scroll_max(&self) -> u16 {
        self.scroll_max
    }
}

/// 在 `index` 处放置块（provider 按序发出，但容错乱序）。
fn insert_block(blocks: &mut Vec<Block>, index: usize, block: Block) {
    if index <= blocks.len() {
        blocks.insert(index, block);
    }
}

/// 条目的可复制纯文本：User/System 取原文；Assistant 取正文块拼接
///（thinking 属模型内部推理，不复制）；Tool 取名称+详情摘要；
/// 空文本返回 `None`。
pub(super) fn item_text(item: &ChatItem) -> Option<String> {
    let text = match item {
        ChatItem::User(text) | ChatItem::System(text) => text.trim().to_string(),
        ChatItem::Assistant(assistant) => assistant
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Text(text) => Some(text.trim()),
                Block::Thinking(_) => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        ChatItem::Tool(tool) => {
            let mut text = format!("{}({})", tool.name, tool.args);
            if !tool.detail.is_empty() {
                text.push('\n');
                text.push_str(&tool.detail.join("\n"));
            }
            text
        }
    };
    (!text.is_empty()).then_some(text)
}

pub(super) fn user_text(content: &UserMessageContent) -> String {
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

// ── skill 手动载入（`skill:<name>`）────────────────────────────────────────

/// 构造手动载入 skill 的注入文本（作为 user 消息进入上下文，随 session 落库）。
///
/// 标签使用 [`ActivatedSkill::prompt_tag`] 的统一格式，与 bootstrap 中 `--skill`
/// 注入 system prompt 的 `<active_skill>` 一致，模型侧无需区分来源。
/// `skill:<name> args` 的附加上下文在消息尾部以 `User: <args>` 追加
///（参考 omp 的 user-invocation 模板），让 skill 能接收调用方意图。
pub(in crate::tui) fn skill_load_message(skill: &ActivatedSkill, args: Option<&str>) -> String {
    let mut message = format!(
        "{}\n\n\
         The user manually loaded this skill into the conversation. \
         Follow its instructions for the subsequent work.",
        skill.prompt_tag()
    );
    if let Some(args) = args {
        use std::fmt::Write as _;
        let _ = write!(message, "\n\nUser: {args}");
    }
    message
}

/// 聊天区压缩展示注入的 skill 消息：返回 `Some` 表示该 user 文本是 skill 注入。
fn skill_load_notice(text: &str) -> Option<String> {
    let tag = parse_active_skill_tag(text)?;
    Some(match tag.path {
        Some(path) => format!("已载入 skill `{}`（{}）", tag.name, path.display()),
        None => format!("已载入 skill `{}`", tag.name),
    })
}

/// mention 展开块的种类（`<active_skill>` / `<file>`）。
#[derive(Debug, Clone, Copy)]
enum MentionBlock {
    Skill,
    File,
}

impl MentionBlock {
    const fn end_tag(self) -> &'static str {
        match self {
            Self::Skill => "</active_skill>",
            Self::File => "</file>",
        }
    }

    /// 块的紧凑标记；无法解析时返回 `None`（调用方原样保留整块）。
    fn chip(self, block: &str) -> Option<String> {
        match self {
            Self::Skill => parse_active_skill_tag(block).map(|tag| format!("@skill:{}", tag.name)),
            Self::File => mention::file_block_path(block).map(|path| format!("@file:{path}")),
        }
    }
}

/// 折叠 mention 展开出的 `<active_skill>` / `<file>` 块为紧凑标记，
/// 避免把大段正文刷进聊天区；无法识别闭合的块原样保留。
fn collapse_mention_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((start, kind)) = next_mention_block(rest) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end_tag = kind.end_tag();
        let Some(end) = tail.find(end_tag) else {
            // 未闭合的块原样保留
            out.push_str(tail);
            return out;
        };
        let block = &tail[..end + end_tag.len()];
        out.push_str(&kind.chip(block).unwrap_or_else(|| block.to_string()));
        rest = &tail[end + end_tag.len()..];
    }
    out.push_str(rest);
    out
}

/// 最近的 mention 块起点（两类块同位起始不可能：前缀不同），取位置靠前者。
fn next_mention_block(text: &str) -> Option<(usize, MentionBlock)> {
    let skill = text
        .find("<active_skill ")
        .map(|i| (i, MentionBlock::Skill));
    let file = text.find("<file ").map(|i| (i, MentionBlock::File));
    match (skill, file) {
        (Some(s), Some(f)) => Some(if s.0 <= f.0 { s } else { f }),
        (Some(s), None) => Some(s),
        (None, Some(f)) => Some(f),
        (None, None) => None,
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

/// 工具结果摘要的最大行数（聊天区保持紧凑，只留尾部上下文）。
const DETAIL_LINES: usize = 3;

/// 提取工具输出的尾部摘要：非空行 trim 后取最后 `DETAIL_LINES` 行，
/// 每行截断到 120 字符（超长由渲染层折行兜底，这里先压住极端长行）。
pub(super) fn result_summary(blocks: &[UserContent]) -> Vec<String> {
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

pub(super) fn assistant_error(
    stop_reason: StopReason,
    error_message: Option<&str>,
) -> Option<String> {
    if matches!(stop_reason, StopReason::Error | StopReason::Aborted) {
        Some(error_message.unwrap_or("未知错误").to_string())
    } else {
        None
    }
}
