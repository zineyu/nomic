//! 聊天区状态：条目列表、流式增量累积、消息游标与滚动。
//!
//! 条目模型（user/assistant/tool/system）与 [`Chat`] 状态结构自持；
//! 复制/VISUAL 选择等操作返回数据，提示语由模式路由层（[`super::App`]）
//! 落到状态栏。

use nomic_ai::{
    AssistantContent, AssistantEvent, Message, StopReason, UserContent, UserMessageContent,
};
use nomic_skills::{ActivatedSkill, parse_active_skill_tag};

use super::step_row;

/// 聊天区条目。
#[derive(Debug)]
pub(in crate::tui) enum ChatItem {
    /// 用户消息
    User(String),
    /// assistant 消息（流式中逐步累积）
    Assistant(AssistantItem),
    /// 一次工具执行
    Tool(ToolItem),
    /// 本地系统提示（slash 命令输出等，不进上下文）
    System(String),
}

impl ChatItem {
    /// 是否为对话消息（user/assistant）：NORMAL `]m`/`[m` 的跳转目标。
    pub(super) const fn is_message(&self) -> bool {
        matches!(self, Self::User(_) | Self::Assistant(_))
    }

    /// 是否为工具调用条目：NORMAL `]t`/`[t` 的跳转目标。
    pub(super) const fn is_tool(&self) -> bool {
        matches!(self, Self::Tool(_))
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

/// 聊天区状态：条目 + 消息游标 + 滚动。不碰交互模式，由 [`super::App`]
/// 组合并按模式路由调用。
#[derive(Debug, Default)]
pub(in crate::tui) struct Chat {
    pub(super) items: Vec<ChatItem>,
    /// NORMAL 的消息游标（items 下标）；进入 NORMAL 时定位到最新一条消息
    pub(super) cursor_item: Option<usize>,
    /// 渲染回写的各条目起始行（draw_chat 折行后同步；未经渲染时为空）
    item_lines: Vec<u16>,
    /// `yc` 代码块循环序号（同一游标消息内重复 yc 依次取下一个块）
    yc_block: usize,
    /// VISUAL 的选择锚点（items 下标；进入 VISUAL 时取消息游标）
    visual_anchor: Option<usize>,
    /// 从底部向上滚动的行数（0 = 跟随最新内容）
    pub(super) scroll: u16,
    /// 聊天区最大可上滚行数（渲染时更新，状态栏滚动位置显示用）
    scroll_max: u16,
}

impl Chat {
    // ── 条目 ────────────────────────────────────────────────────────────────

    /// 聊天区条目（渲染用）。
    pub(in crate::tui) fn items(&self) -> &[ChatItem] {
        &self.items
    }

    /// 追加一条 user 聊天条目；skill 注入消息与压缩摘要消息压缩为系统提示样式的一行。
    pub(super) fn push_user_text(&mut self, text: String) {
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
    pub(in crate::tui) fn push_system(&mut self, text: impl Into<String>) {
        self.items.push(ChatItem::System(text.into()));
        self.scroll_to_bottom();
    }

    /// 清空聊天区（`/new` 开启新对话、`/resume` 恢复前）。
    /// 游标/选择锚点一并重置，保持「游标指向有效条目或 None」的不变量，
    /// 避免残留旧下标越界。
    pub(super) fn clear_items(&mut self) {
        self.items.clear();
        self.cursor_item = None;
        self.visual_anchor = None;
        self.yc_block = 0;
        self.scroll_to_bottom();
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

    /// `/retry`：弹出聊天区尾部失败/未定稿的 assistant 条目（随历史中的
    /// 失败消息一并移除，与 `Agent::retry` 同一口径）。
    pub(super) fn pop_trailing_failed_assistant(&mut self) {
        while matches!(
            self.items.last(),
            Some(ChatItem::Assistant(item)) if item.error.is_some() || !item.done
        ) {
            self.items.pop();
        }
    }

    /// `/copy` 的复制源：聊天区最新一条用户/assistant 消息的纯文本
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

    /// 渲染同步滚动边界：钳制滚动偏移、记录上限，返回生效的滚动偏移。
    /// 聊天区唯一的状态回写通道（状态栏滚动位置显示依赖 `scroll_max`）。
    pub(in crate::tui) fn clamp_scroll(&mut self, max_scroll: u16) -> u16 {
        self.scroll_max = max_scroll;
        self.scroll = self.scroll.min(max_scroll);
        self.scroll
    }

    /// 当前滚动偏移（从底部向上计）。
    pub(in crate::tui) const fn scroll(&self) -> u16 {
        self.scroll
    }

    /// 聊天区最大可上滚行数（渲染同步后有效）。
    pub(in crate::tui) const fn scroll_max(&self) -> u16 {
        self.scroll_max
    }

    // ── 消息游标 ────────────────────────────────────────────────────────────

    /// 渲染回写各条目起始行（draw_chat 折行后同步；测试未经渲染时为空）。
    pub(in crate::tui) fn sync_item_lines(&mut self, starts: Vec<u16>) {
        self.item_lines = starts;
    }

    /// 当前消息游标（items 下标）；是否展示由模式路由层裁决。
    pub(super) const fn cursor(&self) -> Option<usize> {
        self.cursor_item
    }

    /// 游标定位到最早一条对话消息（NORMAL `gg`）。
    pub(super) fn move_cursor_to_first_message(&mut self) {
        self.cursor_item = self.items.iter().position(ChatItem::is_message);
        self.yc_block = 0;
    }

    /// 游标定位到最新一条对话消息（进入 NORMAL、`G`）。
    pub(super) fn move_cursor_to_last_message(&mut self) {
        self.cursor_item = self.items.iter().rposition(ChatItem::is_message);
        self.yc_block = 0;
    }

    /// 移动消息游标到方向上下一个匹配谓词的条目（钳制不循环），并滚动到位。
    pub(super) fn step_cursor(&mut self, direction: isize, matches: fn(&ChatItem) -> bool) {
        let Some(current) = self.cursor_item else {
            return;
        };
        let mut index = current;
        while let Some(next) = step_row(index, direction, self.items.len()) {
            index = next;
            if matches(&self.items[index]) {
                self.focus_item(index);
                return;
            }
        }
    }

    /// 游标聚焦指定条目（重置 `yc` 循环并滚动到位）：搜索命中跳转用。
    pub(super) fn focus_item(&mut self, index: usize) {
        self.cursor_item = Some(index);
        self.yc_block = 0;
        self.scroll_to_cursor_item();
    }

    /// 把消息游标条目滚到视野顶部（渲染同步过行号才生效；未经渲染不动）。
    fn scroll_to_cursor_item(&mut self) {
        let Some(index) = self.cursor_item else {
            return;
        };
        let Some(&line) = self.item_lines.get(index) else {
            return;
        };
        // u16::MAX：条目无可见块（空 assistant），没有可定位的行
        if line != u16::MAX {
            self.scroll = self.scroll_max.saturating_sub(line);
        }
    }

    // ── 复制 ────────────────────────────────────────────────────────────────

    /// NORMAL `yy`：复制消息游标所在条目的纯文本；
    /// `Err` 为状态栏提示语（由模式路由层落到 notice）。
    pub(super) fn copy_cursor_item(&self) -> Result<String, &'static str> {
        let Some(index) = self.cursor_item else {
            return Err("没有可复制的消息");
        };
        self.items
            .get(index)
            .and_then(item_text)
            .ok_or("该条目没有可复制的文本")
    }

    /// NORMAL `yc`：复制游标消息中的 ``` 围栏代码块；多个时按 yc 循环
    /// 依次取下一个。返回（代码块文本, 循环进度提示）。
    pub(super) fn copy_cursor_code_block(
        &mut self,
    ) -> Result<(String, Option<String>), &'static str> {
        let Some(index) = self.cursor_item else {
            return Err("没有可复制的消息");
        };
        let Some(text) = self.items.get(index).and_then(item_text) else {
            return Err("该条目没有可复制的文本");
        };
        let blocks = code_blocks(&text);
        if blocks.is_empty() {
            return Err("该消息中没有代码块");
        }
        let block_index = self.yc_block % blocks.len();
        self.yc_block += 1;
        let progress = (blocks.len() > 1).then(|| {
            format!(
                "已选第 {}/{} 个代码块（重复 yc 循环）",
                block_index + 1,
                blocks.len()
            )
        });
        Ok((blocks[block_index].clone(), progress))
    }

    // ── VISUAL 选择 ─────────────────────────────────────────────────────────

    /// 进入 VISUAL：锚点取消息游标；无可选消息时返回 `false`。
    pub(super) const fn begin_visual(&mut self) -> bool {
        let Some(cursor) = self.cursor_item else {
            return false;
        };
        self.visual_anchor = Some(cursor);
        true
    }

    /// 退出 VISUAL：清除选择锚点。
    pub(super) const fn end_visual(&mut self) {
        self.visual_anchor = None;
    }

    /// 选择范围（锚点与游标的闭区间，小端在前）。
    pub(super) fn visual_range(&self) -> Option<(usize, usize)> {
        let anchor = self.visual_anchor?;
        let cursor = self.cursor_item?;
        Some((anchor.min(cursor), anchor.max(cursor)))
    }

    /// VISUAL `y`：复制锚点到游标的消息范围（各条目纯文本以空行拼接）；
    /// `Err` 为状态栏提示语。
    pub(super) fn yank_visual_range(&self) -> Result<String, &'static str> {
        let Some((start, end)) = self.visual_range() else {
            return Err("没有选择范围");
        };
        let text = self.items[start..=end]
            .iter()
            .filter_map(item_text)
            .collect::<Vec<_>>()
            .join("\n\n");
        if text.is_empty() {
            Err("选中范围没有可复制的文本")
        } else {
            Ok(text)
        }
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

/// 提取文本中的 ``` 围栏代码块内容（依次返回；未闭合的块丢弃）。
fn code_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut in_block = false;
    let mut current = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_block {
                blocks.push(std::mem::take(&mut current));
            }
            in_block = !in_block;
            continue;
        }
        if in_block {
            current.push_str(line);
            current.push('\n');
        }
    }
    blocks
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

// ── skill 手动载入（`/skill:<name>`）────────────────────────────────────────

/// 构造手动载入 skill 的注入文本（作为 user 消息进入上下文，随 session 落库）。
///
/// 标签使用 [`ActivatedSkill::prompt_tag`] 的统一格式，与 bootstrap 中 `--skill`
/// 注入 system prompt 的 `<active_skill>` 一致，模型侧无需区分来源。
/// `/skill:<name> args` 的附加上下文在消息尾部以 `User: <args>` 追加
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
