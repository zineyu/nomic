//! TUI 状态层：聊天条目、流式增量累积、输入编辑、滚动。
//!
//! 对外只暴露语义级操作——按键（[`App::press`] → [`Effect`]）、应用 agent 事件、
//! 滚动、会话与附件管理；编辑器/补全/picker/slash 分发均为模块内部实现。
//! 本模块不碰终端，全部逻辑可脱离 ratatui/crossterm 单测。

use nomic_ai::{
    AssistantContent, AssistantEvent, Message, StopReason, UserContent, UserMessageContent,
};
use nomic_core::{AgentEvent, estimate_context_tokens, usage_context_tokens};
use nomic_prompts::{PromptTemplate, PromptsError};
use nomic_skills::{ActivatedSkill, SkillScope, parse_active_skill_tag};
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

impl ChatItem {
    /// 是否为对话消息（user/assistant）：NORMAL `]m`/`[m` 的跳转目标。
    const fn is_message(&self) -> bool {
        matches!(self, Self::User(_) | Self::Assistant(_))
    }

    /// 是否为工具调用条目：NORMAL `]t`/`[t` 的跳转目标。
    const fn is_tool(&self) -> bool {
        matches!(self, Self::Tool(_))
    }
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
        name: "tree",
        aliases: &[],
        summary: "浏览会话树：选择非工具调用条目作为新分支起点（原分支保留）",
        usage: "/tree",
    },
    SlashCommand {
        name: "compact",
        aliases: &[],
        summary: "压缩上下文为摘要（可带聚焦指令：/compact 专注某部分）",
        usage: "/compact [聚焦指令]",
    },
    SlashCommand {
        name: "retry",
        aliases: &[],
        summary: "重试最近一轮失败的响应（移除失败消息，以原 user 消息重新请求模型）",
        usage: "/retry",
    },
    SlashCommand {
        name: "models",
        aliases: &[],
        summary: "切换模型（跨 provider；推理模型在选择后继续选择思考级别）",
        usage: "/models（选择器）或 /models:<provider>/<模型id>",
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
        name: "copy",
        aliases: &[],
        summary: "复制最新一条消息到剪贴板（assistant 消息取正文，不含 thinking）",
        usage: "/copy",
    },
    SlashCommand {
        name: "thinking",
        aliases: &[],
        summary: "切换 thinking 内容折叠/展开显示（默认折叠）",
        usage: "/thinking",
    },
    SlashCommand {
        name: "goal",
        aliases: &[],
        summary: "开关 goal 模式（默认关闭）：开启后 react loop 停止时若 todo 未全部完成，自动以 user 消息追问",
        usage: "/goal",
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
enum SlashParse {
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
enum SlashAction {
    Help,
    New,
    Resume,
    /// `/tree`：浏览会话树并选择分支起点
    Tree,
    Quit,
    /// `/skill`（None）列出可用 skill；`/skill:<name>` 载入指定 skill
    Skill(Option<String>),
    /// `/compact [聚焦指令]` 手动压缩上下文
    Compact(Option<String>),
    /// `/retry` 重试最近一轮失败的响应
    Retry,
    /// `/models`（None）打开模型选择器；`/models:<provider>/<id>` 直接切换
    Models(Option<String>),
    /// `/image <路径>` 为下一条消息附加图片
    Image(String),
    /// `/copy` 复制最新一条消息到剪贴板
    Copy,
    /// `/thinking` 切换 thinking 内容折叠/展开显示
    Thinking,
    /// `/goal` 开关 goal 模式（loop 停止且 todo 未完成时自动追问）
    Goal,
}

impl SlashAction {
    /// 是否为本地命令：不触碰 agent/driver 状态（不发送 driver job），
    /// 运行中（含工具执行中）可安全执行，不被工具调用阻塞。
    ///
    /// 会话命令（`/new` `/resume` `/tree` `/compact` `/retry` `/models`
    /// `/skill:<name>`）都要经 driver 串行修改 agent 上下文，而 agent 方法
    /// 的调用契约要求非运行状态，因此仍须等本轮结束。
    const fn is_local(&self) -> bool {
        matches!(
            self,
            Self::Help
                | Self::Quit
                | Self::Copy
                | Self::Thinking
                | Self::Goal
                | Self::Skill(None)
                | Self::Image(_)
        )
    }
}

/// 解析一行输入为 slash 命令。
///
/// 参数只支持 `/name:arg` 冒号形式（如 `/skill:jujutsu`）；
/// `/cmd extra` 视为参数形式非法。
fn parse_slash(input: &str) -> SlashParse {
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
    // `/models` 特判：参数是选择项（`<provider>/<模型id>`，不可含空格），
    // `/models id` 与 `/models:id` 两种形式都接受
    if let Some(tail) = rest.strip_prefix("models") {
        if tail.is_empty() {
            return SlashParse::Known(SlashAction::Models(None));
        }
        if let Some(id) = tail.strip_prefix(':').or_else(|| tail.strip_prefix(' ')) {
            let id = id.trim();
            return if id.is_empty() || id.contains(char::is_whitespace) {
                SlashParse::InvalidUsage(models_usage())
            } else {
                SlashParse::Known(SlashAction::Models(Some(id.to_string())))
            };
        }
        // `/modelsxxx`：落入常规解析报未知命令
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
                "tree" if !junk && arg.is_none() => SlashAction::Tree,
                "retry" if !junk && arg.is_none() => SlashAction::Retry,
                "copy" if !junk && arg.is_none() => SlashAction::Copy,
                "thinking" if !junk && arg.is_none() => SlashAction::Thinking,
                "goal" if !junk && arg.is_none() => SlashAction::Goal,
                "quit" if !junk && arg.is_none() => SlashAction::Quit,
                _ => return SlashParse::InvalidUsage(command.usage),
            };
            return SlashParse::Known(action);
        }
    }
    SlashParse::Unknown(name.to_string())
}

/// `/help` 的输出文本。
fn help_text() -> String {
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

/// 补全候选：slash 命令、prompt template 或 `/skill:` 后的 skill 名。
#[derive(Debug)]
pub(super) enum CompletionCandidate {
    Command(&'static SlashCommand),
    /// prompt template（`/name` 调用展开）
    Template(PromptTemplate),
    Skill(SkillEntry),
}

impl CompletionCandidate {
    /// 候选对应的输入片段（不含 `/` 前缀），用于精确匹配、排序与填入。
    fn fragment(&self) -> String {
        match self {
            Self::Command(command) => command.name.to_string(),
            Self::Template(template) => template.name.clone(),
            Self::Skill(entry) => format!("skill:{}", entry.name),
        }
    }

    /// 输入片段是否精确对应该候选（Enter 是否可直接提交）。
    fn matches_fragment(&self, fragment: &str) -> bool {
        match self {
            Self::Command(command) => {
                command.name == fragment || command.aliases.contains(&fragment)
            }
            Self::Template(_) | Self::Skill(_) => self.fragment() == fragment,
        }
    }
}

/// 可用于 `/skill:` 补全的 skill 元数据（从 resolver catalog 快照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkillEntry {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) scope: SkillScope,
}

/// 补全弹层状态：候选列表 + 当前选中项。
#[derive(Debug)]
pub(super) struct Completion {
    pub(super) candidates: Vec<CompletionCandidate>,
    pub(super) selected: usize,
}

/// 选择器的一行：内部 id + 预生成的展示文本（渲染零计算）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PickerRow {
    pub(super) id: String,
    pub(super) text: String,
    /// 是否可选中确认（`/tree` 的工具调用条目只展示不可选）；
    /// 其余选择器恒为 `true`
    pub(super) selectable: bool,
}

/// 选择器种类：决定确认动作（[`Effect`]）与渲染标题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PickerKind {
    /// `/resume`：恢复历史 session
    Resume,
    /// `/tree`：选择分支起点
    Tree,
    /// `/models`：切换模型
    Models,
    /// 模型切换流程第二步：设置思考级别
    Reasoning,
}

/// 选择器状态：候选行 + 当前选中项 + 过滤串（fzf 风格：可打印字符即过滤，
/// ↑/↓ 导航）。`selected` 是**过滤后可见行**的下标。
#[derive(Debug)]
pub(super) struct Picker {
    pub(super) kind: PickerKind,
    pub(super) rows: Vec<PickerRow>,
    pub(super) selected: usize,
    /// 过滤串（空 = 全部可见；大小写不敏感的子串匹配）
    pub(super) filter: String,
}

impl Picker {
    /// 过滤后的可见行（`rows` 下标列表，保持原顺序）。
    pub(super) fn visible(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.rows.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        (0..self.rows.len())
            .filter(|&index| self.rows[index].text.to_lowercase().contains(&needle))
            .collect()
    }
}

/// 暂存的图片附件（`/image <路径>` 载入，随下一条 prompt 一起发送）。
#[derive(Debug)]
struct PendingImage {
    /// 展示名（文件名）
    name: String,
    /// 图片内容块（base64 内联）
    image: nomic_ai::ImageContent,
}

/// PgUp/PgDn 的滚动步长。
const PAGE_SCROLL: u16 = 10;

/// NORMAL 模式 Ctrl+D/Ctrl+U 的半页滚动步长。
const HALF_PAGE_SCROLL: u16 = 5;

/// picker Ctrl+D/Ctrl+U 的半页翻步长（可见行下标计）。
const PICKER_PAGE_SCROLL: isize = 10;

/// TUI 交互模式（ADR-0011）：模式是一等状态，每个按键在当前模式只有一个语义。
///
/// COMMAND 不单列变体：NORMAL 下 `:` 即 INSERT + 预填 `/`（Phase 2 实现）；
/// SEARCH 同理留给 Phase 2。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// 输入（默认）：编辑与提交 prompt、slash 命令、Tab 补全
    Insert,
    /// 浏览：滚动聊天区、跳转、复制；输入字符不进入缓冲（草稿保留）
    Normal,
    /// 搜索：输入框复用为搜索框（增量命中），Enter/Esc 回 NORMAL
    Search,
    /// 可视选择：以消息为粒度选择范围，`y` 复制后回 NORMAL
    Visual,
    /// 选择器打开（`/resume`、`/models`、`/tree`），接管键位。
    /// 派生态：由 `picker.is_some()` 决定，不入 `App::mode` 字段
    Picker,
}

/// 语义化按键：与 crossterm 解耦，保持状态层可脱离终端单测。
/// 由事件循环（mod.rs）从 `KeyEvent` 映射；Ctrl+V 粘贴需异步读剪贴板，
/// 由事件循环拦截处理，不经此枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Key {
    /// 普通字符输入（含 Shift 修饰的可见字符）
    Char(char),
    /// Ctrl+字母（Ctrl+C/D 取消或退出；INSERT 下 Ctrl+W/U/A/E 词级编辑；
    /// NORMAL 下 Ctrl+D/U 半页滚动）
    Ctrl(char),
    /// Alt+字母（INSERT 下 Alt+B/F 词级移动）
    Alt(char),
    Enter,
    /// Shift+Enter 手动换行
    Newline,
    Backspace,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    PageUp,
    PageDown,
    Tab,
    Esc,
}

/// [`App::press`] 返回的语义效果：状态层不持有的外部资源
/// （driver job、取消令牌、session 库、skill resolver、图片加载）
/// 由事件循环接线执行。
#[derive(Debug)]
pub(super) enum Effect {
    /// 提交一轮 prompt（`running` 已置位，避免提交空窗期重复提交）
    Prompt {
        text: String,
        images: Vec<nomic_ai::ImageContent>,
    },
    /// `/compact` 手动压缩上下文（`running` 已置位，Ctrl+C 可取消）
    Compact(Option<String>),
    /// `/retry` 重试最近一轮失败的响应（`running` 已置位，聊天区尾部
    /// 失败/未定稿条目已随历史中的失败消息一并移除）
    Retry,
    /// 取消当前运行（Ctrl+C）
    Cancel,
    /// `/resume`：列出历史 session 并打开选择器
    ListSessions,
    /// picker 确认：恢复选中的 session（加载历史 + 切换落库目标）
    Resume(String),
    /// `/models`：列出当前 provider 的候选模型并打开选择器
    ListModels,
    /// `/models:<provider>/<id>` 或模型选择器确认：切换为指定模型（上下文保留）；
    /// 推理模型由事件循环继续打开思考级别选择器（流程第二步）
    SwitchModel(String),
    /// 思考级别选择器确认：设置思考级别（"off" 关闭）；若有待切换模型
    /// （流程第二步）先应用模型切换
    SetReasoning(String),
    /// 思考级别选择器被 Esc 取消：放弃待切换模型，模型与级别均不变
    CancelModelSwitch,
    /// `/skill`：列出可用 skill 并刷新补全快照
    ListSkills,
    /// `/skill:<name>`：手动载入 skill 到当前对话
    LoadSkill(String),
    /// `/image <路径>`：为下一条消息附加图片
    AttachImage(String),
    /// `/copy`：复制最新一条消息到剪贴板
    CopyText(String),
    /// `/new`：清空上下文开启新对话（新建 session）
    NewSession,
    /// `/tree`：列出当前 session 的会话树并打开选择器
    ListTree,
    /// `/tree` 选择器确认：以所选条目为起点创建分支（恢复该分支上下文，
    /// 后续消息以该条目为父 entry 落库）
    BranchTo(String),
}

/// TUI 应用状态。
// 布尔字段均为相互独立的 UI 开关（运行态/退出/thinking 折叠/goal 模式），
// 两态语义清晰，无需状态机
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub(super) struct App {
    items: Vec<ChatItem>,
    /// 交互模式（ADR-0011）：只取 Insert/Normal；Picker 是派生态
    ///（`picker.is_some()` 时 [`Self::mode`] 返回 Picker），不入此字段
    mode: Mode,
    /// NORMAL 的消息游标（items 下标）；进入 NORMAL 时定位到最新一条消息
    cursor_item: Option<usize>,
    /// 渲染回写的各条目起始行（draw_chat 折行后同步；未经渲染时为空）
    item_lines: Vec<u16>,
    /// `yc` 代码块循环序号（同一游标消息内重复 yc 依次取下一个块）
    yc_block: usize,
    /// VISUAL 的选择锚点（items 下标；进入 VISUAL 时取消息游标）
    visual_anchor: Option<usize>,
    /// 搜索串（NORMAL `/` 进入 SEARCH；Esc 清空，Enter 保留供 n/N）
    search_query: String,
    /// 搜索命中条目（items 下标，升序）
    search_matches: Vec<usize>,
    /// 序列键首键（NORMAL 的 `g`/`[`/`]`/`y`），等待第二键
    pending_key: Option<char>,
    /// 首次进入 NORMAL 的一次性键位提示是否已展示
    normal_hint_shown: bool,
    /// 输入缓冲（可多行，`\n` 为 Shift+Enter 插入的手动换行）
    input: String,
    /// 光标位置（字节索引，始终落在 char 边界）
    cursor: usize,
    /// slash 命令补全弹层（输入以 `/` 开头时出现）
    completion: Option<Completion>,
    /// 选择器（`/resume` / `/models` / `/tree`，打开时接管键位）
    picker: Option<Picker>,
    /// 暂存的图片附件（随下一条 prompt 发送）
    attachments: Vec<PendingImage>,
    /// 从底部向上滚动的行数（0 = 跟随最新内容）
    scroll: u16,
    /// 聊天区最大可上滚行数（渲染时更新，状态栏滚动位置显示用）
    scroll_max: u16,
    running: bool,
    should_quit: bool,
    /// 模型展示名
    model_name: String,
    /// 当前 session id（未持久化时为 None；内部标识，不展示给用户）
    session_id: Option<String>,
    /// 会话标题（首条用户消息的首行摘要；状态栏展示，替代内部 id）
    session_title: Option<String>,
    /// 上下文 token 估算（状态栏用量显示；与自动压缩同一估算口径）
    context_tokens: u64,
    /// 模型上下文窗口（0 = 规格未知，状态栏不显示占比）
    context_window: u64,
    /// 状态栏一次性提示（告警等）
    notice: Option<String>,
    /// spinner 帧序号（仅运行中由事件循环周期推进）
    spinner: usize,
    /// `/skill:` 补全用的可用 skill 快照
    skills: Vec<SkillEntry>,
    /// 可用的 prompt templates（`/name` 调用展开与补全用）
    templates: Vec<PromptTemplate>,
    /// thinking 内容是否折叠显示（默认折叠，`/thinking` 切换）
    thinking_collapsed: bool,
    /// goal 模式（默认关闭，`/goal` 开关）：开启后 react loop 停止且
    /// todo 未全部完成时，由事件循环自动以 user 消息追问
    goal_mode: bool,
}

impl App {
    pub(super) const fn new(
        model_name: String,
        session_id: Option<String>,
        context_window: u64,
    ) -> Self {
        Self {
            items: Vec::new(),
            mode: Mode::Insert,
            cursor_item: None,
            item_lines: Vec::new(),
            yc_block: 0,
            visual_anchor: None,
            search_query: String::new(),
            search_matches: Vec::new(),
            pending_key: None,
            normal_hint_shown: false,
            input: String::new(),
            cursor: 0,
            completion: None,
            picker: None,
            attachments: Vec::new(),
            scroll: 0,
            scroll_max: 0,
            running: false,
            should_quit: false,
            model_name,
            session_id,
            session_title: None,
            context_tokens: 0,
            context_window,
            notice: None,
            spinner: 0,
            skills: Vec::new(),
            templates: Vec::new(),
            thinking_collapsed: true,
            goal_mode: false,
        }
    }

    /// 设置 `/skill:` 补全用的可用 skill 快照（启动时从 resolver catalog 取）。
    pub(super) fn set_available_skills(&mut self, skills: Vec<SkillEntry>) {
        self.skills = skills;
    }

    /// 设置可用的 prompt templates（启动时从 resolver catalog 取）。
    pub(super) fn set_available_templates(&mut self, templates: Vec<PromptTemplate>) {
        self.templates = templates;
    }

    /// 把 resume 恢复的历史消息渲染为聊天条目。
    pub(super) fn load_history(&mut self, messages: &[Message]) {
        self.context_tokens = estimate_context_tokens(messages);
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
                if let Message::Assistant(assistant) = message.as_ref() {
                    // 与 estimate_context_tokens 同一锚点规则：有效响应的实际
                    // usage 即当时上下文总量（错误/中断响应不代表有效上下文）
                    if !matches!(
                        assistant.stop_reason,
                        StopReason::Error | StopReason::Aborted
                    ) {
                        let tokens = usage_context_tokens(&assistant.usage);
                        if tokens > 0 {
                            self.context_tokens = tokens;
                        }
                    }
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

    // ── 按键（语义分发） ────────────────────────────────────────────────────

    /// 当前交互模式（渲染光标/徽标与外部查询用）：picker 打开时派生为
    /// Picker，否则为字段值（Insert/Normal）。
    pub(super) const fn mode(&self) -> Mode {
        if self.picker.is_some() {
            Mode::Picker
        } else {
            self.mode
        }
    }

    /// 消费一个按键，返回需要事件循环接线执行的语义效果。
    /// 按交互模式分发（ADR-0011）：picker/补全/编辑器/slash 的路由全部
    /// 在此内部完成。
    pub(super) fn press(&mut self, key: Key) -> Vec<Effect> {
        match self.mode() {
            // 选择器打开时接管键位（slash 命令仅在空闲时可提交，
            // 此时 agent 必空闲，无运行可取消）
            Mode::Picker => self.press_picker(key),
            Mode::Search => self.press_search(key),
            Mode::Visual => self.press_visual(key),
            Mode::Normal => self.press_normal(key),
            Mode::Insert => self.press_insert(key),
        }
    }

    /// INSERT 模式键位：编辑与提交 prompt、slash 命令、补全。
    fn press_insert(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Ctrl('c' | 'd') => {
                if self.running {
                    vec![Effect::Cancel]
                } else {
                    self.should_quit = true;
                    Vec::new()
                }
            }
            Key::Esc => self.insert_esc(),
            Key::Ctrl('w') => {
                self.delete_word_back();
                Vec::new()
            }
            Key::Ctrl('u') => {
                self.delete_to_line_start();
                Vec::new()
            }
            Key::Ctrl('a') => {
                self.cursor_line_home();
                Vec::new()
            }
            Key::Ctrl('e') => {
                self.cursor_line_end();
                Vec::new()
            }
            Key::Alt('b') => {
                self.cursor_word_left();
                Vec::new()
            }
            Key::Alt('f') => {
                self.cursor_word_right();
                Vec::new()
            }
            Key::Tab => {
                self.tab_complete();
                Vec::new()
            }
            Key::Newline => {
                self.insert_newline();
                Vec::new()
            }
            Key::Enter => self.press_enter(),
            Key::Backspace => {
                self.backspace();
                Vec::new()
            }
            Key::Left => {
                self.cursor_left();
                Vec::new()
            }
            Key::Right => {
                self.cursor_right();
                Vec::new()
            }
            Key::Home => {
                self.cursor_home();
                Vec::new()
            }
            Key::End => {
                self.cursor_end();
                Vec::new()
            }
            // 补全弹层可见时 ↑/↓ 移动选中项，否则滚动聊天区
            Key::Up => {
                self.insert_vertical(-1);
                Vec::new()
            }
            Key::Down => {
                self.insert_vertical(1);
                Vec::new()
            }
            Key::PageUp => {
                self.scroll_up(PAGE_SCROLL);
                Vec::new()
            }
            Key::PageDown => {
                self.scroll_down(PAGE_SCROLL);
                Vec::new()
            }
            Key::Char(c) => {
                self.insert_char(c);
                Vec::new()
            }
            Key::Ctrl(_) | Key::Alt(_) => Vec::new(),
        }
    }

    /// INSERT 的 Esc 退回栈（ADR-0011）：关补全弹层 → 回 NORMAL。
    /// Esc 一律是模式切换，不再取消运行；取消运行由 Ctrl+C 承担。
    fn insert_esc(&mut self) -> Vec<Effect> {
        if !self.dismiss_completion() {
            self.enter_normal();
        }
        Vec::new()
    }

    /// INSERT 的 ↑/↓：补全弹层可见时移动选中项，否则滚动聊天区。
    const fn insert_vertical(&mut self, delta: isize) {
        if self.completion.is_some() {
            self.completion_select(delta);
        } else if delta < 0 {
            self.scroll_up(1);
        } else {
            self.scroll_down(1);
        }
    }

    /// NORMAL 模式键位（ADR-0011）：浏览聊天区与复制；输入字符不进入
    /// 缓冲（草稿保留，`i`/`a`/`Enter` 回到 INSERT 继续编辑）。
    fn press_normal(&mut self, key: Key) -> Vec<Effect> {
        // 序列键第二键；不匹配的键清掉 pending 后照常处理
        //（比 vim 的「无效序列吞键」宽容）
        if let Some(pending) = self.pending_key.take()
            && let Some(effects) = self.normal_sequence(pending, key)
        {
            return effects;
        }
        if let Some(effects) = self.normal_exit(key) {
            return effects;
        }
        match key {
            Key::Char('g') => {
                self.pending_key = Some('g');
                Vec::new()
            }
            Key::Char('[') => {
                self.pending_key = Some('[');
                Vec::new()
            }
            Key::Char(']') => {
                self.pending_key = Some(']');
                Vec::new()
            }
            Key::Char('y') => {
                self.pending_key = Some('y');
                Vec::new()
            }
            Key::Char('d') => {
                self.pending_key = Some('d');
                Vec::new()
            }
            // x：删除草稿光标处字符（草稿编辑，不动消息游标）
            Key::Char('x') => {
                self.delete_char_at_cursor();
                Vec::new()
            }
            Key::Char('G') => {
                self.scroll_to_bottom();
                self.cursor_item = self.items.iter().rposition(ChatItem::is_message);
                self.yc_block = 0;
                Vec::new()
            }
            // V 进入可视选择：锚点取消息游标（无可选消息时提示）
            Key::Char('V') => {
                if self.cursor_item.is_some() {
                    self.visual_anchor = self.cursor_item;
                    self.mode = Mode::Visual;
                } else {
                    self.notice = Some("没有可选择的消息".to_string());
                }
                Vec::new()
            }
            // `/` 进入搜索（输入框复用为搜索框；保留上次查询可编辑）
            Key::Char('/') => {
                self.mode = Mode::Search;
                Vec::new()
            }
            // n/N：在搜索命中条目间循环跳转
            Key::Char('n') => {
                self.search_jump(1);
                Vec::new()
            }
            Key::Char('N') => {
                self.search_jump(-1);
                Vec::new()
            }
            Key::Char('k') | Key::Up => {
                self.scroll_up(1);
                Vec::new()
            }
            Key::Char('j') | Key::Down => {
                self.scroll_down(1);
                Vec::new()
            }
            // Ctrl+D 在 NORMAL 让位 vim 语义（半页滚动）；取消/退出
            // 统一由 Ctrl+C 承担（与 INSERT 的 Ctrl+C 同口径）
            Key::Ctrl('u') => {
                self.scroll_up(HALF_PAGE_SCROLL);
                Vec::new()
            }
            Key::Ctrl('d') => {
                self.scroll_down(HALF_PAGE_SCROLL);
                Vec::new()
            }
            Key::PageUp => {
                self.scroll_up(PAGE_SCROLL);
                Vec::new()
            }
            Key::PageDown => {
                self.scroll_down(PAGE_SCROLL);
                Vec::new()
            }
            // 复制最新一条消息（等价 `/copy`）
            Key::Char('Y') => self.copy_latest(),
            Key::Ctrl('c') => {
                if self.running {
                    vec![Effect::Cancel]
                } else {
                    self.should_quit = true;
                    Vec::new()
                }
            }
            // 其余按键（含普通字符）忽略：不污染输入缓冲
            _ => Vec::new(),
        }
    }

    /// NORMAL 的「离开浏览」键位：`i`/`a`/`Esc` 回 INSERT（光标原位），
    /// `Enter`/`A` 回 INSERT 到输入末尾，`I` 到当前行首，`:` 预填 `/`
    /// 进入命令输入。返回 `Some` 表示已处理。
    fn normal_exit(&mut self, key: Key) -> Option<Vec<Effect>> {
        match key {
            // i/a 回到光标原处继续编辑；Esc 放弃浏览直接返回
            Key::Char('i' | 'a') | Key::Esc => self.leave_normal(),
            // Enter/A 回 INSERT 并把光标置于输入末尾（ADR-0011）；
            // I 回 INSERT 到当前逻辑行首
            Key::Enter | Key::Char('A') => {
                self.leave_normal();
                self.cursor_end();
            }
            Key::Char('I') => {
                self.leave_normal();
                self.cursor_line_home();
            }
            // `:` 进入命令输入：回 INSERT 并预填 `/`（补全弹层随之出现）。
            // 草稿非空时不覆盖用户文本，提示先处理草稿
            Key::Char(':') => {
                let empty = self.input.is_empty();
                self.leave_normal();
                if empty {
                    self.insert_char('/');
                } else {
                    self.notice = Some("草稿非空：i 返回编辑，清空后再用 : 命令".to_string());
                }
            }
            _ => return None,
        }
        Some(Vec::new())
    }

    /// NORMAL 的序列键第二键：`gg` 到顶、`[m`/`]m` 消息跳转、`[t`/`]t`
    /// 工具跳转、`yy`/`yc` 复制、`dd`/`dw` 草稿删除。返回 `Some` 表示
    /// 已处理；`None` 表示组合不匹配，调用方把按键照常分发。
    fn normal_sequence(&mut self, pending: char, key: Key) -> Option<Vec<Effect>> {
        match (pending, key) {
            // gg：到顶（渲染时经 clamp_scroll 钳到实际上限）
            ('g', Key::Char('g')) => {
                self.scroll_up(u16::MAX);
                self.cursor_item = self.items.iter().position(ChatItem::is_message);
                self.yc_block = 0;
            }
            // [m / ]m：上一条/下一条对话消息
            ('[', Key::Char('m')) => self.step_cursor(-1, ChatItem::is_message),
            (']', Key::Char('m')) => self.step_cursor(1, ChatItem::is_message),
            // [t / ]t：上一个/下一个工具调用
            ('[', Key::Char('t')) => self.step_cursor(-1, ChatItem::is_tool),
            (']', Key::Char('t')) => self.step_cursor(1, ChatItem::is_tool),
            // yy：复制游标条目；yc：复制游标消息中的代码块
            ('y', Key::Char('y')) => return Some(self.copy_cursor_item()),
            ('y', Key::Char('c')) => return Some(self.copy_cursor_code_block()),
            // dd：删除草稿当前逻辑行；dw：删除到后一词开头
            ('d', Key::Char('d')) => self.delete_draft_line(),
            ('d', Key::Char('w')) => self.delete_word_forward(),
            _ => return None,
        }
        Some(Vec::new())
    }

    /// VISUAL 模式键位：j/k 以消息为粒度扩展选择，`y` 复制范围后回
    /// NORMAL，Esc 放弃选择回 NORMAL。
    fn press_visual(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Char('j') | Key::Down => self.step_cursor(1, ChatItem::is_message),
            Key::Char('k') | Key::Up => self.step_cursor(-1, ChatItem::is_message),
            Key::Char('y') => {
                let effects = self.yank_visual_selection();
                self.mode = Mode::Normal;
                self.visual_anchor = None;
                return effects;
            }
            Key::Esc => {
                self.mode = Mode::Normal;
                self.visual_anchor = None;
            }
            Key::Ctrl('c') => self.should_quit = true,
            _ => {}
        }
        Vec::new()
    }

    /// VISUAL `y`：复制锚点到游标的消息范围（各条目纯文本以空行拼接）。
    fn yank_visual_selection(&mut self) -> Vec<Effect> {
        let Some((start, end)) = self.visual_range_inner() else {
            self.notice = Some("没有选择范围".to_string());
            return Vec::new();
        };
        let text = self.items[start..=end]
            .iter()
            .filter_map(item_text)
            .collect::<Vec<_>>()
            .join("\n\n");
        if text.is_empty() {
            self.notice = Some("选中范围没有可复制的文本".to_string());
            Vec::new()
        } else {
            vec![Effect::CopyText(text)]
        }
    }

    /// 选择范围（锚点与游标的闭区间，小端在前）。
    fn visual_range_inner(&self) -> Option<(usize, usize)> {
        let anchor = self.visual_anchor?;
        let cursor = self.cursor_item?;
        Some((anchor.min(cursor), anchor.max(cursor)))
    }

    /// 渲染用：VISUAL 的选择范围（仅 VISUAL 下返回）。
    pub(super) fn visual_range(&self) -> Option<(usize, usize)> {
        (self.mode() == Mode::Visual)
            .then_some(self.visual_range_inner())
            .flatten()
    }

    /// SEARCH 模式键位：输入即搜（增量跳转第一个命中），Enter 保留命中
    /// 回 NORMAL（n/N 可继续跳），Esc 清空搜索回 NORMAL。
    fn press_search(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Char(c) => {
                self.search_query.push(c);
                self.refresh_search();
            }
            Key::Backspace => {
                self.search_query.pop();
                self.refresh_search();
            }
            Key::Enter => {
                self.mode = Mode::Normal;
                let count = self.search_matches.len();
                self.notice = Some(if count == 0 {
                    "没有搜索命中".to_string()
                } else {
                    format!("{count} 处命中 · n/N 跳转")
                });
            }
            Key::Esc => {
                self.mode = Mode::Normal;
                self.search_query.clear();
                self.search_matches.clear();
            }
            Key::Ctrl('c') => self.should_quit = true,
            _ => {}
        }
        Vec::new()
    }

    /// 重算搜索命中（输入即搜）：游标跳到当前位置之后（含）的第一个
    /// 命中（循环），无命中保持游标。
    fn refresh_search(&mut self) {
        let query = self.search_query.to_lowercase();
        self.search_matches = if query.is_empty() {
            Vec::new()
        } else {
            self.items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    item_text(item)
                        .filter(|text| text.to_lowercase().contains(&query))
                        .map(|_| index)
                })
                .collect()
        };
        if self.search_matches.is_empty() {
            return;
        }
        let current = self.cursor_item.unwrap_or(0);
        let next = self.search_matches.partition_point(|&m| m < current);
        let next = if next >= self.search_matches.len() {
            0
        } else {
            next
        };
        self.cursor_item = Some(self.search_matches[next]);
        self.yc_block = 0;
        self.scroll_to_cursor_item();
    }

    /// NORMAL `n`/`N`：在搜索命中条目间循环跳转。
    fn search_jump(&mut self, direction: isize) {
        if self.search_matches.is_empty() {
            self.notice = Some("没有搜索命中（NORMAL 下 / 开始搜索）".to_string());
            return;
        }
        let current = self.cursor_item.unwrap_or(0);
        let len = self.search_matches.len();
        let next = if direction > 0 {
            let p = self.search_matches.partition_point(|&m| m <= current);
            if p >= len { 0 } else { p }
        } else {
            let p = self.search_matches.partition_point(|&m| m < current);
            if p == 0 { len - 1 } else { p - 1 }
        };
        self.cursor_item = Some(self.search_matches[next]);
        self.yc_block = 0;
        self.scroll_to_cursor_item();
        self.notice = Some(format!("命中 {}/{len}", next + 1));
    }

    /// 当前搜索串（SEARCH 输入框与命中高亮用）。
    pub(super) fn search_query(&self) -> &str {
        &self.search_query
    }

    /// 搜索命中数（SEARCH 输入框标题用）。
    pub(super) const fn search_match_count(&self) -> usize {
        self.search_matches.len()
    }

    /// 命中高亮词：搜索串非空时返回（Enter 后保留高亮，Esc 清空）。
    pub(super) fn search_highlight(&self) -> Option<&str> {
        (!self.search_query.is_empty()).then_some(self.search_query.as_str())
    }

    /// 进入 NORMAL：草稿保留；消息游标定位到最新一条对话消息；
    /// 首次进入给一次性键位提示。
    fn enter_normal(&mut self) {
        self.mode = Mode::Normal;
        self.pending_key = None;
        self.cursor_item = self.items.iter().rposition(ChatItem::is_message);
        self.yc_block = 0;
        // 防御：退回栈保证进 NORMAL 时弹层已关，这里兜底
        self.completion = None;
        if !self.normal_hint_shown {
            self.normal_hint_shown = true;
            // 一次性提示保持一句话：详细键位由右侧提示与 /help 承担
            self.notice = Some("已进入浏览模式 · i 返回输入".to_string());
        }
    }

    /// 离开 NORMAL 回 INSERT：清掉序列键 pending，避免残留的首键
    /// 在下次进入 NORMAL 时被误当第二键。
    const fn leave_normal(&mut self) {
        self.mode = Mode::Insert;
        self.pending_key = None;
    }

    /// 消息游标（渲染 gutter 高亮用）；浏览类模式（NORMAL/SEARCH/VISUAL）
    /// 下返回。
    pub(super) fn chat_cursor(&self) -> Option<usize> {
        matches!(self.mode(), Mode::Normal | Mode::Search | Mode::Visual)
            .then_some(self.cursor_item)
            .flatten()
    }

    /// 渲染回写各条目起始行（draw_chat 折行后同步；测试未经渲染时为空）。
    pub(super) fn sync_item_lines(&mut self, starts: Vec<u16>) {
        self.item_lines = starts;
    }

    /// 移动消息游标到方向上下一个匹配谓词的条目（钳制不循环），并滚动到位。
    fn step_cursor(&mut self, direction: isize, matches: fn(&ChatItem) -> bool) {
        let Some(current) = self.cursor_item else {
            return;
        };
        let mut index = current;
        while let Some(next) = step_row(index, direction, self.items.len()) {
            index = next;
            if matches(&self.items[index]) {
                self.cursor_item = Some(index);
                self.yc_block = 0;
                self.scroll_to_cursor_item();
                return;
            }
        }
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

    /// NORMAL `yy`：复制消息游标所在条目的纯文本。
    fn copy_cursor_item(&mut self) -> Vec<Effect> {
        let Some(index) = self.cursor_item else {
            self.notice = Some("没有可复制的消息".to_string());
            return Vec::new();
        };
        if let Some(text) = self.items.get(index).and_then(item_text) {
            vec![Effect::CopyText(text)]
        } else {
            self.notice = Some("该条目没有可复制的文本".to_string());
            Vec::new()
        }
    }

    /// NORMAL `yc`：复制游标消息中的 ``` 围栏代码块；多个时按 yc 循环
    /// 依次取下一个。
    fn copy_cursor_code_block(&mut self) -> Vec<Effect> {
        let Some(index) = self.cursor_item else {
            self.notice = Some("没有可复制的消息".to_string());
            return Vec::new();
        };
        let Some(text) = self.items.get(index).and_then(item_text) else {
            self.notice = Some("该条目没有可复制的文本".to_string());
            return Vec::new();
        };
        let blocks = code_blocks(&text);
        if blocks.is_empty() {
            self.notice = Some("该消息中没有代码块".to_string());
            return Vec::new();
        }
        let block_index = self.yc_block % blocks.len();
        self.yc_block += 1;
        if blocks.len() > 1 {
            self.notice = Some(format!(
                "已选第 {}/{} 个代码块（重复 yc 循环）",
                block_index + 1,
                blocks.len()
            ));
        }
        vec![Effect::CopyText(blocks[block_index].clone())]
    }

    /// 复制最新一条消息到剪贴板（`/copy` 与 NORMAL `Y` 共用）。
    fn copy_latest(&mut self) -> Vec<Effect> {
        if let Some(text) = self.latest_message_text() {
            vec![Effect::CopyText(text)]
        } else {
            self.notice = Some("没有可复制的消息".to_string());
            Vec::new()
        }
    }

    /// picker 打开时的键位（fzf 风格）：可打印字符即过滤，↑/↓ 与
    /// Ctrl+N/P 移动，Home/End 跳首/尾，Ctrl+D/U 半页翻，Enter 确认；
    /// Esc 先清过滤、再关闭；Ctrl+C 保持全局退出。
    fn press_picker(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Up | Key::Ctrl('p') => self.picker_select(-1),
            Key::Down | Key::Ctrl('n') => self.picker_select(1),
            Key::Ctrl('u') => self.picker_select(-PICKER_PAGE_SCROLL),
            Key::Ctrl('d') => self.picker_select(PICKER_PAGE_SCROLL),
            Key::Home => self.picker_jump(0, 1),
            Key::End => {
                let last = self
                    .picker
                    .as_ref()
                    .map_or(0, |picker| picker.visible().len().saturating_sub(1));
                self.picker_jump(last, -1);
            }
            Key::Esc => {
                // 有过滤串先清过滤（留在 picker），否则关闭
                if self.picker_clear_filter() {
                    return Vec::new();
                }
                // 思考级别选择器是模型切换流程的第二步：Esc 还需放弃
                // 事件循环侧暂存的待切换模型
                let abort_switch = matches!(
                    self.picker,
                    Some(Picker {
                        kind: PickerKind::Reasoning,
                        ..
                    })
                );
                self.close_picker();
                if abort_switch {
                    return vec![Effect::CancelModelSwitch];
                }
            }
            Key::Backspace => self.picker_pop_filter(),
            Key::Ctrl('c') => self.should_quit = true,
            Key::Enter => {
                if let Some((kind, id)) = self.take_picker_selection() {
                    return vec![match kind {
                        PickerKind::Resume => Effect::Resume(id),
                        PickerKind::Tree => Effect::BranchTo(id),
                        PickerKind::Models => Effect::SwitchModel(id),
                        PickerKind::Reasoning => Effect::SetReasoning(id),
                    }];
                }
            }
            // 可打印字符即过滤（含 j/k/q——导航全部走箭头/Ctrl 键，一键一义）
            Key::Char(c) => {
                if let Some(picker) = &mut self.picker {
                    picker.filter.push(c);
                    picker.selected = 0;
                }
                self.picker_snap_selection();
            }
            _ => {}
        }
        Vec::new()
    }

    /// 清空 picker 过滤串；返回是否确有过滤串被清空
    ///（Esc 据此决定清过滤还是关 picker）。
    fn picker_clear_filter(&mut self) -> bool {
        let Some(picker) = &mut self.picker else {
            return false;
        };
        if picker.filter.is_empty() {
            return false;
        }
        picker.filter.clear();
        picker.selected = 0;
        self.picker_snap_selection();
        true
    }

    /// 删除 picker 过滤串末字符（Backspace）。
    fn picker_pop_filter(&mut self) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if picker.filter.pop().is_some() {
            picker.selected = 0;
            self.picker_snap_selection();
        }
    }

    /// 选中项对齐到最近的可选行（过滤变化后调用）：从当前位置向下找，
    /// 找不到再向上。
    fn picker_snap_selection(&mut self) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let visible = picker.visible();
        if visible.is_empty() {
            return;
        }
        let pos = picker.selected.min(visible.len() - 1);
        let snapped = (pos..visible.len())
            .chain((0..pos).rev())
            .find(|&p| picker.rows[visible[p]].selectable);
        picker.selected = snapped.unwrap_or(pos);
    }

    /// 跳转选中到可见行的 `pos`，不可选时沿 `direction` 找可选行。
    fn picker_jump(&mut self, pos: usize, direction: isize) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let visible = picker.visible();
        if visible.is_empty() {
            return;
        }
        let mut pos = pos.min(visible.len() - 1);
        while !picker.rows[visible[pos]].selectable {
            let Some(next) = step_row(pos, direction, visible.len()) else {
                return;
            };
            pos = next;
        }
        picker.selected = pos;
    }

    /// Enter：补全弹层未精确匹配时先填入候选；否则取出输入，
    /// 按 slash 命令或普通 prompt 分发（运行中的口径见
    /// [`Self::press_enter_running`]）。
    fn press_enter(&mut self) -> Vec<Effect> {
        if self.accept_completion_on_enter() {
            // 已填入补全候选；再次 Enter 提交
            return Vec::new();
        }
        if self.running {
            return self.press_enter_running();
        }
        let Some(text) = self.take_input() else {
            if self.has_attachments() {
                self.notice = Some("已附加图片，输入文本后 Enter 一起发送".to_string());
            }
            return Vec::new();
        };
        match parse_slash(&text) {
            SlashParse::NotCommand => {
                let images = self.take_attachments();
                // AgentStart 事件也会置位；先置避免提交空窗期重复提交
                self.running = true;
                self.notice = None;
                vec![Effect::Prompt { text, images }]
            }
            SlashParse::Known(action) => self.execute_slash(action),
            SlashParse::InvalidUsage(usage) => {
                self.notice = Some(format!("参数形式不对，用法：{usage}"));
                Vec::new()
            }
            SlashParse::Unknown(name) => self.submit_template(&text, &name),
        }
    }

    /// 运行中（含工具执行中）的 Enter：本地 slash 命令照常执行——
    /// 它们不触碰 agent/driver 状态（[`SlashAction::is_local`]），
    /// 长时间运行的工具调用不应阻塞它们；会话命令、prompt 与模板调用
    /// 仍须等本轮结束（输入保留，结束后可再提交）。
    fn press_enter_running(&mut self) -> Vec<Effect> {
        let text = self.input.trim().to_string();
        if !text.is_empty()
            && let SlashParse::Known(action) = parse_slash(&text)
            && action.is_local()
        {
            self.take_input();
            self.notice = None;
            return self.execute_slash(action);
        }
        self.notice = Some(
            "运行中，等待本轮结束（/help、/copy、/thinking、/goal、/skill、/image、/quit 不受影响）"
                .to_string(),
        );
        Vec::new()
    }

    /// 未知 slash 命令：按 prompt template 调用尝试展开提交；未命中模板时
    /// 维持未知命令提示。内建命令优先（已在 `parse_slash` 中匹配）。
    fn submit_template(&mut self, text: &str, name: &str) -> Vec<Effect> {
        match nomic_prompts::expand_invocation(&self.templates, text) {
            Ok(Some(expanded)) => {
                let images = self.take_attachments();
                // 与普通 prompt 同一口径：先置 running 避免提交空窗期重复提交
                self.running = true;
                self.notice = None;
                vec![Effect::Prompt {
                    text: expanded,
                    images,
                }]
            }
            Err(PromptsError::UnterminatedQuote { .. }) => {
                self.notice = Some("参数形式不对：引号未闭合".to_string());
                Vec::new()
            }
            _ => {
                self.notice = Some(format!("未知命令 /{name}，输入 /help 查看可用命令"));
                Vec::new()
            }
        }
    }

    /// slash 命令的内部处置：能就地完成的直接做，需要外部资源的转为效果。
    fn execute_slash(&mut self, action: SlashAction) -> Vec<Effect> {
        match action {
            SlashAction::Help => {
                self.push_system(help_text());
                Vec::new()
            }
            SlashAction::Quit => {
                self.should_quit = true;
                Vec::new()
            }
            SlashAction::Compact(instructions) => {
                // 压缩是一次 LLM 调用：按 mini-run 处理，Ctrl+C 可取消
                self.running = true;
                self.notice = None;
                vec![Effect::Compact(instructions)]
            }
            SlashAction::Retry => {
                // 与 Agent::retry 同一口径：聊天区尾部失败/未定稿的 assistant
                // 条目随历史中的失败消息一并移除；是否实际重跑由 driver 回执
                // 告知（agent 历史是唯一权威，这里不做预判定）
                while matches!(
                    self.items.last(),
                    Some(ChatItem::Assistant(item)) if item.error.is_some() || !item.done
                ) {
                    self.items.pop();
                }
                self.running = true;
                self.notice = None;
                vec![Effect::Retry]
            }
            SlashAction::Resume => vec![Effect::ListSessions],
            SlashAction::Models(None) => vec![Effect::ListModels],
            SlashAction::Models(Some(id)) => vec![Effect::SwitchModel(id)],
            SlashAction::Skill(None) => vec![Effect::ListSkills],
            SlashAction::Skill(Some(name)) => vec![Effect::LoadSkill(name)],
            SlashAction::Image(path) => vec![Effect::AttachImage(path)],
            SlashAction::Copy => self.copy_latest(),
            SlashAction::Thinking => {
                self.thinking_collapsed = !self.thinking_collapsed;
                let state = if self.thinking_collapsed {
                    "已折叠"
                } else {
                    "已展开"
                };
                self.push_system(format!("thinking 显示：{state}（/thinking 切换）"));
                Vec::new()
            }
            SlashAction::Goal => {
                self.goal_mode = !self.goal_mode;
                let state = if self.goal_mode {
                    "已开启：react loop 停止时若 todo 未全部完成，将自动以 user 消息追问"
                } else {
                    "已关闭"
                };
                self.push_system(format!("goal 模式{state}（/goal 切换）"));
                Vec::new()
            }
            SlashAction::New => vec![Effect::NewSession],
            SlashAction::Tree => vec![Effect::ListTree],
        }
    }

    // ── 运行生命周期 ────────────────────────────────────────────────────────

    /// 一轮运行（prompt/压缩）结束：回到空闲态，按需置状态栏告警。
    pub(super) fn finish_run(&mut self, notice: Option<String>) {
        self.running = false;
        self.notice = notice;
    }

    /// 开始一轮自动运行（goal 模式追问）：与 prompt 提交同一口径
    /// 先置 running，避免 AgentStart 事件到达前的空窗期重复提交。
    pub(super) fn begin_run(&mut self) {
        self.running = true;
        self.notice = None;
    }

    /// 置状态栏一次性提示（告警等）。
    pub(super) fn warn(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    // ── 会话操作 ────────────────────────────────────────────────────────────

    /// `/skill`：刷新补全快照并列出可用 skill（本地展示，不进上下文）。
    pub(super) fn show_skills(&mut self, skills: Vec<SkillEntry>) {
        self.push_system(skill_list_text(&skills));
        self.skills = skills;
    }

    /// `/new`：清空聊天区开启新对话；session 切换由调用方随后经
    /// [`Self::set_session`] / [`Self::warn`] 回报。
    pub(super) fn start_new_conversation(&mut self) {
        self.clear_items();
        self.context_tokens = 0;
        self.push_system("已开启新对话，上下文已清空。");
    }

    /// 切换当前 session 标识（`/new` 新建或 `/resume` 恢复后）。
    pub(super) fn set_session(&mut self, session_id: String) {
        self.session_id = Some(session_id);
    }

    /// `/resume`：以恢复的历史消息替换聊天区并切换 session。
    pub(super) fn restore_conversation(&mut self, messages: &[Message], session_id: String) {
        self.clear_items();
        self.load_history(messages);
        self.session_id = Some(session_id);
    }

    /// `/tree` 选择器确认：以分支重放的消息替换聊天区（session 不变；
    /// 落库父指针切换由调用方随后完成）。
    pub(super) fn restore_branch(&mut self, messages: &[Message]) {
        self.clear_items();
        self.load_history(messages);
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

    fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.refresh_completion();
    }

    /// 粘贴一段文本到光标处（可含换行；`\r\n` 统一为 `\n`），随后重算补全。
    pub(super) fn paste_text(&mut self, text: &str) {
        // 粘贴的意图是编辑：NORMAL 下先回到 INSERT（草稿保留）
        self.mode = Mode::Insert;
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.is_empty() {
            return;
        }
        self.input.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.refresh_completion();
    }

    /// Shift+Enter 手动换行：换行是空白字符，补全弹层随之关闭。
    fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    fn backspace(&mut self) {
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

    fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.input[..self.cursor]
                .char_indices()
                .last()
                .map_or(0, |(index, _)| index);
            self.refresh_completion();
        }
    }

    fn cursor_right(&mut self) {
        if let Some(c) = self.input[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
            self.refresh_completion();
        }
    }

    fn cursor_home(&mut self) {
        self.cursor = 0;
        self.refresh_completion();
    }

    fn cursor_end(&mut self) {
        self.cursor = self.input.len();
        self.refresh_completion();
    }

    /// Ctrl+A：光标移到当前逻辑行开头（多行输入只作用当前行）。
    fn cursor_line_home(&mut self) {
        let start = self.input[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        if start != self.cursor {
            self.cursor = start;
            self.refresh_completion();
        }
    }

    /// Ctrl+E：光标移到当前逻辑行末尾（多行输入只作用当前行）。
    fn cursor_line_end(&mut self) {
        let end = self.input[self.cursor..]
            .find('\n')
            .map_or(self.input.len(), |offset| self.cursor + offset);
        if end != self.cursor {
            self.cursor = end;
            self.refresh_completion();
        }
    }

    /// Ctrl+U：删除到当前逻辑行开头（多行输入只清当前行）。
    fn delete_to_line_start(&mut self) {
        let start = self.input[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        if start < self.cursor {
            self.input.replace_range(start..self.cursor, "");
            self.cursor = start;
            self.refresh_completion();
        }
    }

    /// Ctrl+W：删除光标前的一个词（连同词前的空白间隔）。
    fn delete_word_back(&mut self) {
        let target = self.word_left_pos();
        if target < self.cursor {
            self.input.replace_range(target..self.cursor, "");
            self.cursor = target;
            self.refresh_completion();
        }
    }

    /// Alt+B：光标移到前一个词的开头。
    fn cursor_word_left(&mut self) {
        let target = self.word_left_pos();
        if target != self.cursor {
            self.cursor = target;
            self.refresh_completion();
        }
    }

    /// 光标前一词开头的字节索引：先跳过非词字符（空白/标点间隔），
    /// 再跳过词字符。
    fn word_left_pos(&self) -> usize {
        let mut target = self.cursor;
        let mut in_word = false;
        for (index, c) in self.input[..self.cursor].char_indices().rev() {
            if is_word_char(c) {
                in_word = true;
            } else if in_word {
                break;
            }
            target = index;
        }
        target
    }

    /// Alt+F：光标移到后一个词的开头（先跳过当前所在的词，再跳过词间隔）。
    fn cursor_word_right(&mut self) {
        let target = self.word_right_pos();
        if target != self.cursor {
            self.cursor = target;
            self.refresh_completion();
        }
    }

    /// 光标后一词开头的字节索引（Alt+F 与 NORMAL `dw` 共用）。
    fn word_right_pos(&self) -> usize {
        let after = &self.input[self.cursor..];
        // 光标在词中时先跳过该词剩余部分
        let rest = if after.chars().next().is_some_and(is_word_char) {
            let word_len: usize = after
                .chars()
                .take_while(|&c| is_word_char(c))
                .map(char::len_utf8)
                .sum();
            &after[word_len..]
        } else {
            after
        };
        let gap_len: usize = rest
            .chars()
            .take_while(|&c| !is_word_char(c))
            .map(char::len_utf8)
            .sum();
        self.input.len() - rest.len() + gap_len
    }

    /// NORMAL `x`：删除草稿光标处字符（光标不动）。
    fn delete_char_at_cursor(&mut self) {
        if let Some(c) = self.input[self.cursor..].chars().next() {
            self.input
                .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
            self.refresh_completion();
        }
    }

    /// NORMAL `dw`：删除到后一词开头（光标不动）。
    fn delete_word_forward(&mut self) {
        let target = self.word_right_pos();
        if target > self.cursor {
            self.input.replace_range(self.cursor..target, "");
            self.refresh_completion();
        }
    }

    /// NORMAL `dd`：删除草稿当前逻辑行（连同其换行；单行即清空草稿）。
    fn delete_draft_line(&mut self) {
        if self.input.is_empty() {
            return;
        }
        let mut start = self.input[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let end = self.input[self.cursor..]
            .find('\n')
            .map_or(self.input.len(), |offset| self.cursor + offset + 1)
            .min(self.input.len());
        // 末行没有后随换行：连同前置换行，避免留下空尾行
        if end == self.input.len() && start > 0 {
            start -= 1;
        }
        self.input.replace_range(start..end, "");
        self.cursor = start.min(self.input.len());
        self.refresh_completion();
    }

    /// 取出待提交的输入并清空缓冲；空输入返回 `None`。
    fn take_input(&mut self) -> Option<String> {
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
    fn take_attachments(&mut self) -> Vec<nomic_ai::ImageContent> {
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
            self.command_candidates(&fragment)
        };
    }

    /// slash 命令与 prompt template 候选（按名称/别名前缀匹配，按名称排序；
    /// 同名时内建命令在前）。
    fn command_candidates(&self, fragment: &str) -> Option<Completion> {
        let mut candidates: Vec<CompletionCandidate> = SLASH_COMMANDS
            .iter()
            .filter(|command| {
                command.name.starts_with(fragment)
                    || command.aliases.iter().any(|a| a.starts_with(fragment))
            })
            .map(CompletionCandidate::Command)
            .collect();
        candidates.extend(
            self.templates
                .iter()
                .filter(|template| template.name.starts_with(fragment))
                .cloned()
                .map(CompletionCandidate::Template),
        );
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
    fn tab_complete(&mut self) {
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
    const fn completion_select(&mut self, delta: isize) {
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
    fn dismiss_completion(&mut self) -> bool {
        self.completion.take().is_some()
    }

    /// Enter 且补全弹层可见时的智能接受：输入未精确匹配任何候选时
    /// 填入选中候选（返回 `true`，不提交）；已精确匹配则返回 `false` 正常提交。
    fn accept_completion_on_enter(&mut self) -> bool {
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

    // ── 选择器（/resume、/models、/tree 共用） ──────────────────────────────

    /// 打开 `/resume` 选择器（从头选中）；调用方保证候选非空。
    pub(super) fn open_resume_picker(&mut self, rows: Vec<PickerRow>) {
        debug_assert!(!rows.is_empty());
        self.picker = Some(Picker {
            kind: PickerKind::Resume,
            rows,
            selected: 0,
            filter: String::new(),
        });
    }

    /// 打开 `/models` 选择器，预选中当前模型；调用方保证候选非空。
    pub(super) fn open_model_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        debug_assert!(!rows.is_empty());
        debug_assert!(selected < rows.len());
        self.picker = Some(Picker {
            kind: PickerKind::Models,
            rows,
            selected,
            filter: String::new(),
        });
    }

    /// 打开思考级别选择器（模型切换流程第二步，预选中当前级别）；
    /// 调用方保证候选非空。
    pub(super) fn open_reasoning_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        debug_assert!(!rows.is_empty());
        debug_assert!(selected < rows.len());
        self.picker = Some(Picker {
            kind: PickerKind::Reasoning,
            rows,
            selected,
            filter: String::new(),
        });
    }

    /// 打开 `/tree` 选择器（预选中 `selected`，通常是当前分支末端）；
    /// 调用方保证候选非空且 `selected` 落在可选行上。
    pub(super) fn open_tree_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        debug_assert!(!rows.is_empty());
        debug_assert!(rows[selected].selectable);
        self.picker = Some(Picker {
            kind: PickerKind::Tree,
            rows,
            selected,
            filter: String::new(),
        });
    }

    /// 当前选择器（渲染与键位路由用）。
    pub(super) const fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    /// 关闭选择器（Esc 取消）。
    fn close_picker(&mut self) {
        self.picker = None;
    }

    /// 移动选中项（在过滤后的可见行上到底/顶钳制，不循环；跳过不可选行）。
    fn picker_select(&mut self, delta: isize) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let visible = picker.visible();
        if visible.is_empty() {
            return;
        }
        let direction: isize = delta.signum();
        let mut pos = picker.selected.min(visible.len() - 1);
        for _ in 0..delta.unsigned_abs() {
            let Some(next) = step_row(pos, direction, visible.len()) else {
                break;
            };
            pos = next;
        }
        // 落点不可选时沿移动方向继续；该方向上没有更多可选行则保持原位
        while !picker.rows[visible[pos]].selectable {
            let Some(next) = step_row(pos, direction, visible.len()) else {
                return;
            };
            pos = next;
        }
        picker.selected = pos;
    }

    /// Enter 确认：取出选中行的（种类, id）并关闭选择器。
    /// 过滤后无可见行或选中不可选行（`/tree` 的工具调用条目）时不确认、
    /// 保持打开。
    fn take_picker_selection(&mut self) -> Option<(PickerKind, String)> {
        let picker = self.picker.as_ref()?;
        let visible = picker.visible();
        let &row = visible.get(picker.selected)?;
        if !picker.rows[row].selectable {
            return None;
        }
        let picker = self.picker.take()?;
        Some((picker.kind, picker.rows[row].id.clone()))
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
            // 首条真实用户消息生成会话标题（skill 注入/压缩摘要不作标题）
            if self.session_title.is_none() {
                let title = nomic_session::first_line(&text);
                if !title.is_empty() {
                    self.session_title = Some(title);
                }
            }
            self.items.push(ChatItem::User(text));
        }
        self.scroll_to_bottom();
    }

    /// `/copy` 的复制源：聊天区最新一条用户/assistant 消息的纯文本
    ///（[`item_text`] 口径）；全部为空返回 `None`。
    fn latest_message_text(&self) -> Option<String> {
        self.items
            .iter()
            .rev()
            .filter(|item| item.is_message())
            .find_map(item_text)
    }

    /// 追加一条本地系统提示（不进上下文、不落库）。
    pub(super) fn push_system(&mut self, text: impl Into<String>) {
        self.items.push(ChatItem::System(text.into()));
        self.scroll_to_bottom();
    }

    /// 清空聊天区（`/new` 开启新对话、`/resume` 恢复前）；会话标题随
    /// 聊天区重建（`load_history` 会由恢复的首条用户消息重新生成）。
    fn clear_items(&mut self) {
        self.items.clear();
        self.session_title = None;
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

    const fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }

    /// 渲染同步滚动边界：钳制滚动偏移、记录上限，返回生效的滚动偏移。
    /// 聊天区唯一的状态回写通道（状态栏滚动位置显示依赖 `scroll_max`）。
    pub(super) fn clamp_scroll(&mut self, max_scroll: u16) -> u16 {
        self.scroll_max = max_scroll;
        self.scroll = self.scroll.min(max_scroll);
        self.scroll
    }

    // ── 渲染读接口 ──────────────────────────────────────────────────────────

    /// 聊天区条目（渲染用）。
    pub(super) fn items(&self) -> &[ChatItem] {
        &self.items
    }

    /// 是否有 agent 运行在途（spinner 动画与运行态渲染用）。
    pub(super) const fn is_running(&self) -> bool {
        self.running
    }

    /// 是否请求退出（事件循环退出条件）。
    pub(super) const fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// thinking 内容是否折叠显示（`/thinking` 切换）。
    pub(super) const fn thinking_collapsed(&self) -> bool {
        self.thinking_collapsed
    }

    /// goal 模式是否开启（`/goal` 开关，默认关闭）。
    pub(super) const fn goal_mode(&self) -> bool {
        self.goal_mode
    }

    /// 模型展示名。
    pub(super) fn model_name(&self) -> &str {
        &self.model_name
    }

    /// `/models` 切换成功后更新状态栏的模型徽标与上下文窗口。
    pub(super) fn set_model(&mut self, name: String, context_window: u64) {
        self.model_name = name;
        self.context_window = context_window;
    }

    /// 当前 session id（未持久化时为 None；内部标识，不对用户展示）。
    #[cfg(test)]
    pub(super) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// 状态栏：会话标题（首条用户消息摘要；无则回退「新会话」/「无 session」）。
    pub(super) fn session_label(&self) -> String {
        if self.session_id.is_none() {
            return "无 session".to_string();
        }
        self.session_title
            .clone()
            .unwrap_or_else(|| "新会话".to_string())
    }

    /// 状态栏：当前上下文 token 估算。
    pub(super) const fn context_tokens(&self) -> u64 {
        self.context_tokens
    }

    /// 状态栏：模型上下文窗口（0 = 规格未知）。
    pub(super) const fn context_window(&self) -> u64 {
        self.context_window
    }

    /// 更新上下文 token 估算（driver 每个 job 后回报，事件循环接线）。
    pub(super) const fn set_context_tokens(&mut self, tokens: u64) {
        self.context_tokens = tokens;
    }

    /// 状态栏一次性提示。
    pub(super) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// 当前滚动偏移（从底部向上计）。
    pub(super) const fn scroll(&self) -> u16 {
        self.scroll
    }

    /// 聊天区最大可上滚行数（渲染同步后有效）。
    pub(super) const fn scroll_max(&self) -> u16 {
        self.scroll_max
    }

    /// 附件展示名列表（输入框附件行渲染用）。
    pub(super) fn attachment_names(&self) -> impl Iterator<Item = &str> {
        self.attachments.iter().map(|pending| pending.name.as_str())
    }
}

/// 在 `index` 处放置块（provider 按序发出，但容错乱序）。
fn insert_block(blocks: &mut Vec<Block>, index: usize, block: Block) {
    if index <= blocks.len() {
        blocks.insert(index, block);
    }
}

/// 选择器逐行步进：越过边界返回 `None`（钳制语义由调用方决定）。
fn step_row(index: usize, direction: isize, len: usize) -> Option<usize> {
    let next = index.checked_add_signed(direction)?;
    (next < len).then_some(next)
}

/// 词字符判定（INSERT 词级移动/删除共用）：字母数字与下划线。
/// CJK 字符的 `is_alphanumeric` 为真，连续中文视为一个长词。
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 条目的可复制纯文本：User/System 取原文；Assistant 取正文块拼接
///（thinking 属模型内部推理，不复制）；Tool 取名称+详情摘要；
/// 空文本返回 `None`。
fn item_text(item: &ChatItem) -> Option<String> {
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

/// `/image` 的用法提示（以 SLASH_COMMANDS 为唯一来源）。
fn image_usage() -> &'static str {
    SLASH_COMMANDS
        .iter()
        .find(|command| command.name == "image")
        .map_or("/image:<路径>", |command| command.usage)
}

fn models_usage() -> &'static str {
    SLASH_COMMANDS
        .iter()
        .find(|command| command.name == "models")
        .map_or("/models:<provider>/<模型id>", |command| command.usage)
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
/// 标签使用 [`ActivatedSkill::prompt_tag`] 的统一格式，与 bootstrap 中 `--skill`
/// 注入 system prompt 的 `<active_skill>` 一致，模型侧无需区分来源。
pub(super) fn skill_load_message(skill: &ActivatedSkill) -> String {
    format!(
        "{}\n\n\
         The user manually loaded this skill into the conversation. \
         Follow its instructions for the subsequent work.",
        skill.prompt_tag()
    )
}

/// `/skill` 无参时展示的可用 skill 清单（本地展示，不进上下文）。
fn skill_list_text(skills: &[SkillEntry]) -> String {
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
            skill.name, skill.description, skill.scope
        );
    }
    text
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
// 测试数据包含模板占位符字面量（${2:-nomic} 等），并非格式化参数
#[allow(clippy::literal_string_with_formatting_args)]
mod tests {
    use std::path::PathBuf;

    use nomic_ai::{ApiKind, AssistantMessage, TextContent, ThinkingContent, Usage, UserMessage};
    use nomic_core::{ToolResult, ToolUpdate};
    use nomic_skills::SkillScope;

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

    /// 有效 assistant 响应的 usage 即上下文总量锚点；错误/中断响应不作锚点。
    #[test]
    fn message_end_usage_updates_context_tokens() {
        let mut app = app();
        assert_eq!(app.context_tokens(), 0);

        let mut message = assistant_message(Vec::new(), StopReason::Stop, None);
        let Message::Assistant(assistant) = message.as_mut() else {
            unreachable!()
        };
        assistant.usage = Usage {
            total_tokens: 12_345,
            ..Usage::default()
        };
        app.handle_event(&AgentEvent::MessageEnd(message));
        assert_eq!(app.context_tokens(), 12_345);

        let mut failed = assistant_message(Vec::new(), StopReason::Error, Some("boom".to_string()));
        let Message::Assistant(assistant) = failed.as_mut() else {
            unreachable!()
        };
        assistant.usage = Usage {
            total_tokens: 99_000,
            ..Usage::default()
        };
        app.handle_event(&AgentEvent::MessageEnd(failed));
        assert_eq!(app.context_tokens(), 12_345);
    }

    /// 恢复历史（启动 / `/resume`）时按估算口径初始化上下文用量。
    #[test]
    fn load_history_estimates_context_tokens() {
        let mut app = app();
        let text = "a".repeat(400);
        app.load_history(&[*user_message(&text)]);
        assert_eq!(app.context_tokens(), 100);
    }

    /// `/new` 开启新对话时上下文用量清零。
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
        app.picker_select(1);
        app.picker_select(1);
        app.picker_select(1);
        assert_eq!(app.picker().expect("picker").selected, 2);
        app.picker_select(-5);
        assert_eq!(app.picker().expect("picker").selected, 0);

        // Enter 确认：返回选中 id 并关闭；关闭后再次确认为 None
        app.picker_select(1);
        assert_eq!(
            app.take_picker_selection(),
            Some((PickerKind::Resume, "id-1".to_string()))
        );
        assert!(app.picker().is_none());
        assert!(app.take_picker_selection().is_none());
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
        assert_eq!(parse_slash("/copy"), SlashParse::Known(SlashAction::Copy));
        assert_eq!(
            parse_slash("/thinking"),
            SlashParse::Known(SlashAction::Thinking)
        );
        assert_eq!(parse_slash("/goal"), SlashParse::Known(SlashAction::Goal));
        assert_eq!(parse_slash("/retry"), SlashParse::Known(SlashAction::Retry));
        assert_eq!(
            parse_slash("/foobar"),
            SlashParse::Unknown("foobar".to_string())
        );
        // 首尾空白容错
        assert_eq!(parse_slash("  /new  "), SlashParse::Known(SlashAction::New));
    }

    #[test]
    fn copy_takes_latest_message_text() {
        let mut app = app();
        // 空聊天区：无可复制内容，就地提示
        assert!(app.execute_slash(SlashAction::Copy).is_empty());
        assert_eq!(app.notice.as_deref(), Some("没有可复制的消息"));

        app.items.push(ChatItem::User("第一条问题".to_string()));
        app.items.push(ChatItem::Assistant(AssistantItem {
            blocks: vec![
                Block::Thinking("内部推理".to_string()),
                Block::Text("第一段正文".to_string()),
                Block::Text("第二段正文".to_string()),
            ],
            done: true,
            error: None,
        }));
        // thinking 不复制，多个正文块以空行连接
        let [Effect::CopyText(text)] = &app.execute_slash(SlashAction::Copy)[..] else {
            panic!("expected CopyText effect");
        };
        assert_eq!(text, "第一段正文\n\n第二段正文");

        // 最新一条是只有工具调用的 assistant 消息：向前找有正文的消息
        app.items
            .push(ChatItem::Assistant(AssistantItem::default()));
        app.items.push(ChatItem::User("最新问题".to_string()));
        let [Effect::CopyText(text)] = &app.execute_slash(SlashAction::Copy)[..] else {
            panic!("expected CopyText effect");
        };
        assert_eq!(text, "最新问题");
    }

    #[test]
    fn thinking_toggles_collapse_state() {
        let mut app = app();
        // 默认折叠，本地命令不产生外部效果
        assert!(app.thinking_collapsed());
        assert!(app.execute_slash(SlashAction::Thinking).is_empty());
        assert!(!app.thinking_collapsed());
        assert!(app.execute_slash(SlashAction::Thinking).is_empty());
        assert!(app.thinking_collapsed());
        // 每次切换在聊天区留下系统提示
        let systems = app
            .items
            .iter()
            .filter(|item| matches!(item, ChatItem::System(_)))
            .count();
        assert_eq!(systems, 2);
        // 本地命令：运行中也可执行
        assert!(SlashAction::Thinking.is_local());
    }

    #[test]
    fn goal_toggles_mode_state() {
        let mut app = app();
        // 默认关闭，本地命令不产生外部效果
        assert!(!app.goal_mode());
        assert!(app.execute_slash(SlashAction::Goal).is_empty());
        assert!(app.goal_mode());
        assert!(app.execute_slash(SlashAction::Goal).is_empty());
        assert!(!app.goal_mode());
        // 每次切换在聊天区留下系统提示
        let systems = app
            .items
            .iter()
            .filter(|item| matches!(item, ChatItem::System(_)))
            .count();
        assert_eq!(systems, 2);
        // 本地命令：运行中也可执行
        assert!(SlashAction::Goal.is_local());
    }

    #[test]
    fn retry_pops_trailing_failed_assistant_and_requests_retry() {
        let mut app = app();
        app.handle_event(&AgentEvent::MessageStart(user_message("hi")));
        app.handle_event(&AgentEvent::MessageStart(assistant_message(
            Vec::new(),
            StopReason::Error,
            Some("boom".to_string()),
        )));
        app.handle_event(&AgentEvent::MessageEnd(assistant_message(
            Vec::new(),
            StopReason::Error,
            Some("boom".to_string()),
        )));

        let effects = app.execute_slash(SlashAction::Retry);

        // 失败条目随历史中的失败消息一并移除；提交重试请求并进入运行态
        assert!(matches!(&effects[..], [Effect::Retry]));
        assert!(app.running);
        assert_eq!(app.items.len(), 1);
        assert!(matches!(app.items[0], ChatItem::User(_)));
    }

    #[test]
    fn retry_pops_unfinished_assistant_item() {
        // 流协议错误路径：MessageStart 后没有 MessageEnd 的未定稿条目同样移除
        let mut app = app();
        app.handle_event(&AgentEvent::MessageStart(user_message("hi")));
        app.handle_event(&AgentEvent::MessageStart(assistant_message(
            Vec::new(),
            StopReason::Stop,
            None,
        )));

        let effects = app.execute_slash(SlashAction::Retry);

        assert!(matches!(&effects[..], [Effect::Retry]));
        assert_eq!(app.items.len(), 1);
        assert!(matches!(app.items[0], ChatItem::User(_)));
    }

    #[test]
    fn retry_after_success_keeps_items_and_delegates() {
        // 是否可重试由 agent 判定（历史是唯一权威）：成功条目保留，照常提交
        let mut app = app();
        app.handle_event(&AgentEvent::MessageStart(user_message("hi")));
        app.handle_event(&AgentEvent::MessageStart(assistant_message(
            vec![text_block("ok")],
            StopReason::Stop,
            None,
        )));
        app.handle_event(&AgentEvent::MessageEnd(assistant_message(
            vec![text_block("ok")],
            StopReason::Stop,
            None,
        )));

        let effects = app.execute_slash(SlashAction::Retry);

        assert!(matches!(&effects[..], [Effect::Retry]));
        assert_eq!(app.items.len(), 2);
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
            parse_slash("/goal x"),
            SlashParse::InvalidUsage(_)
        ));
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
                scope: SkillScope::Project,
            },
            SkillEntry {
                name: "rust-review".to_string(),
                description: "review rust".to_string(),
                scope: SkillScope::AgentUser,
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

    // ── press 语义分发（新接口） ────────────────────────────────────────────

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

    #[test]
    fn enter_submits_prompt_with_attachments_and_marks_running() {
        let mut app = app();
        app.stage_image("a.png".to_string(), image());
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
        assert!(!app.has_attachments());
        assert_eq!(app.input(), "");
    }

    #[test]
    fn template_completion_lists_templates_with_commands() {
        let mut prefixed = app();
        prefixed.set_available_templates(vec![
            template("review", "Review $@", Some("<path>")),
            template("component", "Create $1", None),
        ]);
        for c in "/re".chars() {
            prefixed.insert_char(c);
        }
        let completion = prefixed.completion().expect("前缀弹出候选");
        assert_eq!(
            candidate_fragments(completion),
            vec!["resume", "retry", "review"]
        );

        // Tab 填入首个候选（接受后候选收敛到精确匹配，再次 Tab 不变）
        prefixed.tab_complete();
        assert_eq!(prefixed.input(), "/resume");
        prefixed.tab_complete();
        assert_eq!(prefixed.input(), "/resume");

        // 唯一前缀时 Tab 直接填入模板候选
        let mut unique = app();
        unique.set_available_templates(vec![template("review", "Review $@", Some("<path>"))]);
        for c in "/rev".chars() {
            unique.insert_char(c);
        }
        assert_eq!(
            candidate_fragments(unique.completion().expect("唯一候选")),
            vec!["review"]
        );
        unique.tab_complete();
        assert_eq!(unique.input(), "/review");

        // 空片段时模板与内建命令一起出现
        let mut empty = app();
        empty.set_available_templates(vec![template("zz-top", "body", None)]);
        empty.insert_char('/');
        let completion = empty.completion().expect("全部候选");
        assert!(candidate_fragments(completion).contains(&"zz-top".to_string()));
    }

    #[test]
    fn enter_expands_template_invocation_into_prompt() {
        let mut spaced = app();
        spaced.set_available_templates(vec![template("greet", "Hello $1, from ${2:-nomic}", None)]);
        spaced.paste_text("/greet world \"a b\"");
        let effects = spaced.press(Key::Enter);
        assert!(spaced.is_running());
        let [Effect::Prompt { text, images }] = &effects[..] else {
            panic!("expected single Prompt effect");
        };
        assert_eq!(text, "Hello world, from a b");
        assert!(images.is_empty());

        // 冒号形式同样展开
        let mut colon = app();
        colon.set_available_templates(vec![template("greet", "Hello $1", None)]);
        colon.paste_text("/greet:world");
        let [Effect::Prompt { text, .. }] = &colon.press(Key::Enter)[..] else {
            panic!("expected single Prompt effect");
        };
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn template_invocation_errors_and_builtin_precedence() {
        let mut quoted = app();
        quoted.set_available_templates(vec![
            template("greet", "Hello $1", None),
            // 与内建命令同名的模板不抢占 /help
            template("help", "template help", None),
        ]);
        // 引号未闭合：提示参数形式不对，不提交
        quoted.paste_text("/greet \"unterminated");
        assert!(quoted.press(Key::Enter).is_empty());
        assert!(!quoted.is_running());
        assert_eq!(quoted.notice.as_deref(), Some("参数形式不对：引号未闭合"));

        // 未知命令：维持原提示
        let mut missing = app();
        missing.paste_text("/missing arg");
        assert!(missing.press(Key::Enter).is_empty());
        assert!(
            missing
                .notice
                .as_deref()
                .is_some_and(|notice| notice.contains("未知命令 /missing"))
        );

        // 内建命令优先于同名模板
        let mut builtin = app();
        builtin.set_available_templates(vec![template("help", "template help", None)]);
        builtin.paste_text("/help");
        assert!(builtin.press(Key::Enter).is_empty());
        assert!(!builtin.is_running());
        assert!(
            matches!(&builtin.items.last(), Some(ChatItem::System(text)) if text.contains("可用命令"))
        );
    }

    #[test]
    fn enter_while_running_only_warns() {
        let mut app = app();
        app.handle_event(&AgentEvent::AgentStart);
        app.paste_text("hi");
        assert!(app.press(Key::Enter).is_empty());
        assert!(app.notice().is_some_and(|n| n.contains("运行中")));
        // 输入保留，待运行结束后可再提交
        assert_eq!(app.input(), "hi");
    }

    /// 运行中（含工具执行中）：本地 slash 命令照常执行，不被工具调用阻塞。
    #[test]
    fn enter_while_running_allows_local_slash_commands() {
        let mut app = app();
        app.handle_event(&AgentEvent::AgentStart);

        // /help 就地输出可用命令，不产生效果
        app.paste_text("/help");
        assert!(app.press(Key::Enter).is_empty());
        assert!(
            matches!(app.items.last(), Some(ChatItem::System(text)) if text.contains("可用命令"))
        );
        assert_eq!(app.input(), "");

        // /copy 返回 CopyText 效果（复制源为聊天区最新消息）
        app.items.push(ChatItem::User("已发的消息".to_string()));
        app.paste_text("/copy");
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::CopyText(text)] if text == "已发的消息"));

        // /quit 运行中同样生效
        app.paste_text("/quit");
        assert!(app.press(Key::Enter).is_empty());
        assert!(app.should_quit());
    }

    /// 运行中：会话命令（经 driver 修改 agent 上下文）仍须等本轮结束，
    /// 输入保留供结束后提交。
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
            app.paste_text(input);
            assert!(
                app.press(Key::Enter).is_empty(),
                "{input} 运行中不应产生效果"
            );
            assert!(
                app.notice().is_some_and(|n| n.contains("运行中")),
                "{input} 应提示运行中"
            );
            assert_eq!(app.input(), input, "{input} 输入应保留");
            app.take_input();
        }
    }

    /// 运行中补全弹层未精确匹配时 Enter 先填入候选，再次 Enter 执行本地命令。
    #[test]
    fn enter_while_running_accepts_completion_before_dispatch() {
        let mut app = app();
        app.handle_event(&AgentEvent::AgentStart);
        app.paste_text("/he");
        assert!(app.completion().is_some());
        // 第一次 Enter：填入补全候选，不提交
        assert!(app.press(Key::Enter).is_empty());
        assert_eq!(app.input(), "/help");
        // 第二次 Enter：精确匹配，执行本地命令
        assert!(app.press(Key::Enter).is_empty());
        assert!(
            matches!(app.items.last(), Some(ChatItem::System(text)) if text.contains("可用命令"))
        );
    }

    #[test]
    fn slash_new_returns_effect_and_start_new_conversation_resets() {
        let mut app = app();
        app.push_system("旧内容");
        app.paste_text("/new");
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::NewSession]));
        assert!(!app.is_running());
        // 事件循环执行效果：重置聊天区并切换 session
        app.start_new_conversation();
        app.set_session("new-id".to_string());
        assert_eq!(app.items().len(), 1);
        assert!(matches!(&app.items()[0], ChatItem::System(t) if t.contains("新对话")));
        assert_eq!(app.session_id(), Some("new-id"));
    }

    #[test]
    fn compact_returns_effect_with_instructions_and_marks_running() {
        let mut app = app();
        app.paste_text("/compact 专注测试");
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::Compact(Some(i))] if i == "专注测试"));
        assert!(app.is_running());
    }

    #[test]
    fn ctrl_c_cancels_when_running_and_quits_when_idle() {
        let mut idle = app();
        assert!(idle.press(Key::Ctrl('c')).is_empty());
        assert!(idle.should_quit());

        let mut running = app();
        running.handle_event(&AgentEvent::AgentStart);
        let effects = running.press(Key::Ctrl('c'));
        assert!(matches!(&effects[..], [Effect::Cancel]));
        assert!(!running.should_quit());
    }

    /// Esc 退回栈（ADR-0011）：关补全弹层 → 回 NORMAL（运行中亦然）。
    /// Esc 只做无损的模式切换；取消运行由 Ctrl+C 承担。
    #[test]
    fn esc_retreat_stack() {
        // 运行中：Esc 不取消运行，进 NORMAL 浏览；Ctrl+C 才取消
        let mut running = app();
        running.handle_event(&AgentEvent::AgentStart);
        assert!(running.press(Key::Esc).is_empty());
        assert_eq!(running.mode(), Mode::Normal);
        assert!(running.is_running(), "Esc 不影响运行");
        assert!(matches!(
            &running.press(Key::Ctrl('c'))[..],
            [Effect::Cancel]
        ));

        // 1. 弹层开着：关弹层，留在 INSERT
        let mut app = app();
        app.paste_text("/h");
        assert!(app.completion().is_some());
        assert!(app.press(Key::Esc).is_empty());
        assert!(app.completion().is_none());
        assert_eq!(app.mode(), Mode::Insert);
        assert_eq!(app.input(), "/h", "输入不受 Esc 影响");

        // 2. 空闲：进 NORMAL；一次性提示只出现一次
        assert!(app.press(Key::Esc).is_empty());
        assert_eq!(app.mode(), Mode::Normal);
        assert!(app.notice().is_some(), "首次进 NORMAL 给一次性提示");
        app.press(Key::Char('i'));
        app.warn("其他提示");
        app.press(Key::Esc);
        assert_eq!(app.notice(), Some("其他提示"), "一次性提示只出现一次");
    }

    /// NORMAL：j/k 滚动，字符不污染输入缓冲（草稿保留），
    /// i 回原光标、Enter 到输入末尾返回 INSERT。
    #[test]
    fn normal_mode_navigates_and_preserves_draft() {
        let mut app = app();
        app.paste_text("草稿内容");
        let draft_len = app.input().len();
        app.press(Key::Esc);
        assert_eq!(app.mode(), Mode::Normal);

        // 字符不进入缓冲；j/k 滚动
        assert!(app.press(Key::Char('x')).is_empty());
        assert_eq!(app.input(), "草稿内容");
        app.press(Key::Char('k'));
        assert_eq!(app.scroll(), 1);
        app.press(Key::Char('j'));
        assert_eq!(app.scroll(), 0);

        // i 回 INSERT，草稿与光标位置保留
        assert!(app.press(Key::Char('i')).is_empty());
        assert_eq!(app.mode(), Mode::Insert);
        assert_eq!(app.input(), "草稿内容");

        // Enter 回 INSERT：光标到输入末尾（「草稿内容」4 个 CJK 字符，宽 8 列）
        app.press(Key::Home);
        app.press(Key::Esc);
        app.press(Key::Enter);
        assert_eq!(app.mode(), Mode::Insert);
        let (row, col) = app.cursor_position();
        assert_eq!((row, col), (0, 8), "光标在末尾：{row},{col}");
        assert_eq!(app.input().len(), draft_len);
    }

    /// NORMAL：gg 到顶、G 回底（跟随模式）、Ctrl+D/U 半页滚动；
    /// g 后接非 g 键吞掉 pending，该键照常处理。
    #[test]
    fn normal_mode_gg_g_and_half_page_scroll() {
        let mut app = app();
        app.press(Key::Esc);

        app.press(Key::Char('g'));
        app.press(Key::Char('g'));
        assert_eq!(app.scroll(), u16::MAX, "gg 滚到顶（渲染时钳到上限）");

        app.press(Key::Char('G'));
        assert_eq!(app.scroll(), 0, "G 回底");

        app.press(Key::Ctrl('u'));
        assert_eq!(app.scroll(), 5);
        app.press(Key::Ctrl('d'));
        assert_eq!(app.scroll(), 0);

        // g + j：pending 清除，j 照常滚动
        app.press(Key::Char('g'));
        app.press(Key::Char('j'));
        assert_eq!(app.scroll(), 0, "j 向下滚动钳在 0");
        app.press(Key::Char('g'));
        app.press(Key::Char('k'));
        assert_eq!(app.scroll(), 1, "k 正常上滚，未被 gg 吞掉");
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

    /// NORMAL：Ctrl+C 与 INSERT 同口径（取消/退出）；Ctrl+D 让位半页滚动。
    #[test]
    fn normal_mode_ctrl_c_quits_ctrl_d_scrolls() {
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
        let cursor_col = |app: &App| app.cursor_position().1;

        // Ctrl+W：删前一个词连同词前空白
        {
            let mut app = app();
            app.paste_text("hello world  foo");
            app.press(Key::Ctrl('w'));
            assert_eq!(app.input(), "hello world  ");
            app.press(Key::Ctrl('w'));
            assert_eq!(app.input(), "hello ", "连空白间隔一起删");
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
        assert_eq!(app.cursor_position(), (1, 0), "Ctrl+A 到当前行首");
        app.press(Key::Ctrl('e'));
        assert_eq!(app.cursor_position(), (1, 11), "Ctrl+E 到当前行尾");
        app.press(Key::Ctrl('u'));
        assert_eq!(app.input(), "first line\n", "Ctrl+U 只清当前行");
        assert_eq!(app.cursor_position(), (1, 0));
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
        assert_eq!(app.input(), "草稿追加");
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
        assert_eq!(app.input(), "");
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

    /// NORMAL `:`：空草稿时预填 `/` 进入命令输入（补全弹层自动出现）；
    /// 草稿非空时不覆盖用户文本，提示先处理。
    #[test]
    fn normal_colon_prefills_slash_when_draft_empty() {
        let mut drafting = app();
        drafting.paste_text("未发送的草稿");
        drafting.press(Key::Esc);
        assert!(drafting.press(Key::Char(':')).is_empty());
        assert_eq!(drafting.mode(), Mode::Insert);
        assert_eq!(drafting.input(), "未发送的草稿", "不覆盖草稿");
        assert!(drafting.notice().is_some());

        let mut app = app();
        app.press(Key::Esc);
        assert!(app.press(Key::Char(':')).is_empty());
        assert_eq!(app.mode(), Mode::Insert);
        assert_eq!(app.input(), "/");
        assert!(app.completion().is_some(), "命令补全弹层自动出现");
    }

    /// 构造含工具调用的历史：user → assistant → tool → assistant。
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

    /// NORMAL 消息游标：进入时定位最新一条消息；]m/[m 在消息间移动（跳过
    /// 工具与系统条目），]t/[t 在工具条目间移动；越界钳制。
    #[test]
    fn normal_cursor_steps_between_messages_and_tools() {
        let mut app = app_with_history();
        app.press(Key::Esc);
        // 条目布局：0 user, 1 assistant, 2 tool, 3 user, 4 assistant
        assert_eq!(app.cursor_item, Some(4), "进入 NORMAL 定位最新一条消息");

        // [m 逐条向前：assistant → user（跳过 tool）
        app.press(Key::Char('['));
        app.press(Key::Char('m'));
        assert_eq!(app.cursor_item, Some(3));
        app.press(Key::Char('['));
        app.press(Key::Char('m'));
        assert_eq!(app.cursor_item, Some(1), "跳过 tool 条目");
        // ]m 回到尾部
        app.press(Key::Char(']'));
        app.press(Key::Char('m'));
        assert_eq!(app.cursor_item, Some(3));

        // [t 定位工具条目；继续 [t 越界钳制在原位
        app.press(Key::Char('['));
        app.press(Key::Char('t'));
        assert_eq!(app.cursor_item, Some(2));
        app.press(Key::Char('['));
        app.press(Key::Char('t'));
        assert_eq!(app.cursor_item, Some(2), "没有更早的工具条目，钳制");

        // gg/G：游标随滚动到首/尾消息
        app.press(Key::Char('g'));
        app.press(Key::Char('g'));
        assert_eq!(app.cursor_item, Some(0));
        app.press(Key::Char('G'));
        assert_eq!(app.cursor_item, Some(4));
    }

    /// NORMAL `yy`：复制游标条目纯文本（assistant 取正文块拼接，不含 thinking）。
    #[test]
    fn normal_yy_copies_cursor_item() {
        let mut app = app_with_history();
        app.press(Key::Esc);
        let effects = {
            app.press(Key::Char('y'));
            app.press(Key::Char('y'))
        };
        assert!(
            matches!(&effects[..], [Effect::CopyText(text)] if text.contains("看这里")),
            "{effects:?}"
        );

        // 游标移到 user 条目：复制 user 文本
        app.press(Key::Char('['));
        app.press(Key::Char('m'));
        let effects = {
            app.press(Key::Char('y'));
            app.press(Key::Char('y'))
        };
        assert!(matches!(&effects[..], [Effect::CopyText(text)] if text == "第二个问题"));
    }

    /// NORMAL `yc`：复制游标消息中的代码块；多个循环选择，没有则提示。
    #[test]
    fn normal_yc_copies_code_blocks_with_cycle() {
        let mut app = app_with_history();
        app.press(Key::Esc);
        let copy = |app: &mut App| {
            app.press(Key::Char('y'));
            app.press(Key::Char('c'))
        };
        // 第一块
        let effects = copy(&mut app);
        assert!(
            matches!(&effects[..], [Effect::CopyText(text)] if text == "fn main() {}\n"),
            "{effects:?}"
        );
        // 同一游标消息上重复 yc：循环到第二块，并给出进度提示
        let effects = copy(&mut app);
        assert!(matches!(&effects[..], [Effect::CopyText(text)] if text == "第二块\n"));
        assert_eq!(app.notice(), Some("已选第 2/2 个代码块（重复 yc 循环）"));
        // 循环回第一块
        let effects = copy(&mut app);
        assert!(matches!(&effects[..], [Effect::CopyText(text)] if text == "fn main() {}\n"));

        // 无代码块的消息：提示
        app.press(Key::Char('['));
        app.press(Key::Char('m'));
        assert!(copy(&mut app).is_empty());
        assert_eq!(app.notice(), Some("该消息中没有代码块"));
    }

    /// NORMAL `/` 搜索：输入即搜（增量跳第一个命中），Enter 保留命中
    /// 供 n/N 循环跳转，Esc 清空搜索与高亮。
    #[test]
    fn normal_slash_search_incremental_and_jump() {
        let mut app = app_with_history();
        app.press(Key::Esc);

        // 输入即搜：「问题」命中两条 user 消息（下标 0、3）
        app.press(Key::Char('/'));
        assert_eq!(app.mode(), Mode::Search);
        for c in "问题".chars() {
            app.press(Key::Char(c));
        }
        assert_eq!(app.search_matches, vec![0, 3]);
        assert_eq!(app.cursor_item, Some(0), "游标在尾部，增量回绕到首个命中");

        // Enter 保留命中；n 循环跳转
        app.press(Key::Enter);
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.notice(), Some("2 处命中 · n/N 跳转"));
        app.press(Key::Char('n'));
        assert_eq!(app.cursor_item, Some(3), "n 循环到下一处");
        app.press(Key::Char('n'));
        assert_eq!(app.cursor_item, Some(0));
        // N 反向
        app.press(Key::Char('N'));
        assert_eq!(app.cursor_item, Some(3));

        // 再次 / 保留上次查询可编辑；Esc 清空
        app.press(Key::Char('/'));
        assert_eq!(app.search_query(), "问题");
        app.press(Key::Backspace);
        assert_eq!(app.search_query(), "问");
        app.press(Key::Esc);
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.search_query(), "");
        assert!(app.search_highlight().is_none(), "Esc 清空高亮");
        // 无命中时 n 给提示
        assert!(app.press(Key::Char('n')).is_empty());
        assert_eq!(app.notice(), Some("没有搜索命中（NORMAL 下 / 开始搜索）"));
    }

    /// VISUAL：V 以游标为锚点进入，j/k 以消息为粒度扩展选择，y 复制
    /// 范围后回 NORMAL；Esc 放弃；无可选消息时提示。
    #[test]
    fn visual_selects_message_range_and_yanks() {
        // 无消息时 V 提示
        let mut empty = app();
        empty.press(Key::Esc);
        assert!(empty.press(Key::Char('V')).is_empty());
        assert_eq!(empty.mode(), Mode::Normal);
        assert_eq!(empty.notice(), Some("没有可选择的消息"));

        let mut app = app_with_history();
        app.press(Key::Esc);

        // 锚点取游标（最新 assistant，下标 4），k 扩展两条（到 tool 前的
        // user 1 再上一跳越过 tool 到 assistant 1）
        app.press(Key::Char('V'));
        assert_eq!(app.mode(), Mode::Visual);
        assert_eq!(app.visual_range(), Some((4, 4)));
        app.press(Key::Char('k'));
        app.press(Key::Char('k'));
        assert_eq!(app.cursor_item, Some(1), "k 逐消息上移，跳过 tool");
        assert_eq!(app.visual_range(), Some((1, 4)));

        // y 复制范围：assistant/user/tool/user/assistant 各条目文本拼接
        let effects = app.press(Key::Char('y'));
        let [Effect::CopyText(text)] = &effects[..] else {
            panic!("应产出 CopyText：{effects:?}");
        };
        assert!(text.contains("第一个回答"), "{text}");
        assert!(text.contains("第二个问题"), "{text}");
        assert!(text.contains("bash(ls)"), "工具条目文本也在范围内：{text}");
        assert_eq!(app.mode(), Mode::Normal, "y 后回 NORMAL");
        assert_eq!(app.visual_range(), None);

        // Esc 放弃选择
        app.press(Key::Char('V'));
        app.press(Key::Char('k'));
        assert!(app.press(Key::Esc).is_empty());
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.visual_range(), None);
    }

    /// NORMAL 草稿编辑：x 删光标处字符、dw 删到后一词、dd 删当前逻辑行、
    /// A/I 分别到末尾/行首回 INSERT；消息游标与聊天区不受影响。
    #[test]
    fn normal_draft_editing() {
        // dd：多行草稿删当前逻辑行（连同换行）
        let mut multiline = app();
        multiline.paste_text("first\nsecond\nthird");
        multiline.press(Key::Esc);
        // 光标在末尾（third 行）：删末行连同前置换行
        multiline.press(Key::Char('d'));
        multiline.press(Key::Char('d'));
        assert_eq!(multiline.input(), "first\nsecond");
        // 光标回行首，再 dd 两次：逐行删清
        multiline.press(Key::Char('d'));
        multiline.press(Key::Char('d'));
        assert_eq!(multiline.input(), "first");
        multiline.press(Key::Char('d'));
        multiline.press(Key::Char('d'));
        assert_eq!(multiline.input(), "");
        // 空草稿上 dd/x 安全无操作
        multiline.press(Key::Char('d'));
        multiline.press(Key::Char('d'));
        multiline.press(Key::Char('x'));
        assert_eq!(multiline.input(), "");

        let mut app = app();
        app.paste_text("hello world foo");
        app.press(Key::Home);
        app.press(Key::Esc);
        assert_eq!(app.mode(), Mode::Normal);

        // x：删除光标处字符（光标在行首，删 'h'）
        app.press(Key::Char('x'));
        assert_eq!(app.input(), "ello world foo");
        assert_eq!(app.cursor_position().1, 0);

        // dw：删到后一词开头（"ello "）
        app.press(Key::Char('d'));
        app.press(Key::Char('w'));
        assert_eq!(app.input(), "world foo");

        // A：回 INSERT 到末尾；I：回 INSERT 到行首
        app.press(Key::Char('A'));
        assert_eq!(app.mode(), Mode::Insert);
        assert_eq!(app.cursor_position().1, 9);
        app.press(Key::Esc);
        app.press(Key::Char('I'));
        assert_eq!(app.mode(), Mode::Insert);
        assert_eq!(app.cursor_position().1, 0);
    }

    /// 消息游标滚动定位：渲染回写条目行号后，移动游标滚动到该条目。
    #[test]
    fn cursor_movement_scrolls_to_item() {
        let mut app = app_with_history();
        // 模拟渲染回写：条目 0..=4 起始行 0,10,20,30,40；scroll_max 50
        app.sync_item_lines(vec![0, 10, 20, 30, 40]);
        app.clamp_scroll(50);
        app.press(Key::Esc);
        assert_eq!(app.cursor_item, Some(4));
        app.press(Key::Char('['));
        app.press(Key::Char('m'));
        // 条目 3 起始行 30：scroll = 50 - 30
        assert_eq!(app.scroll(), 20);
        app.press(Key::Char('g'));
        app.press(Key::Char('g'));
        assert_eq!(app.scroll(), u16::MAX, "gg 仍然直接滚到顶");
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
        assert_eq!(picker.selected, 0);

        // ↓ 在过滤结果上移动，Enter 确认命中行
        app.press(Key::Down);
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::Resume(id)] if id == "s3"));
        assert!(app.picker().is_none());

        // Esc 先清过滤、再关闭
        app.open_resume_picker(rows());
        app.press(Key::Char('x'));
        assert_eq!(app.picker().expect("picker").filter, "x");
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
        assert_eq!(app.picker().expect("picker").selected, 29);
        app.press(Key::Home);
        assert_eq!(app.picker().expect("picker").selected, 0);
        app.press(Key::Ctrl('d'));
        assert_eq!(app.picker().expect("picker").selected, 10);
        app.press(Key::Ctrl('u'));
        assert_eq!(app.picker().expect("picker").selected, 0);

        // g/G 普通过滤字符（不过滤语言引入序列键，一键一义）
        app.press(Key::Char('g'));
        assert_eq!(app.picker().expect("picker").filter, "g");
    }

    /// `/models` 解析：无参打开选择器，带 id（空格或冒号）直接切换，
    /// id 含空白报用法错误。
    #[test]
    fn parse_slash_models_forms() {
        assert_eq!(
            parse_slash("/models"),
            SlashParse::Known(SlashAction::Models(None))
        );
        assert_eq!(
            parse_slash("/models:gpt-5.2"),
            SlashParse::Known(SlashAction::Models(Some("gpt-5.2".to_string())))
        );
        assert_eq!(
            parse_slash("/models gpt-5.2"),
            SlashParse::Known(SlashAction::Models(Some("gpt-5.2".to_string())))
        );
        assert!(matches!(
            parse_slash("/models a b"),
            SlashParse::InvalidUsage(_)
        ));
        assert_eq!(
            parse_slash("/modelsx"),
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
        assert_eq!(app.picker().expect("picker").selected, 1);
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

    /// `/models` 选择器：预选中当前模型，Enter 产出 SwitchModel 效果。
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
        assert_eq!(app.picker().expect("picker").selected, 1);
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::SwitchModel(id)] if id == "m2"));
        assert!(app.picker().is_none());
    }

    /// `/models` 无参 → ListModels 效果；切换成功后状态栏模型信息更新。
    #[test]
    fn models_slash_effects_and_set_model_updates_status() {
        let mut app = app();
        app.paste_text("/models");
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::ListModels]));

        app.paste_text("/models:gpt-5.2");
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::SwitchModel(id)] if id == "gpt-5.2"));

        app.set_model("GPT-5.2".to_string(), 400_000);
        assert_eq!(app.model_name(), "GPT-5.2");
        assert_eq!(app.context_window(), 400_000);
    }

    #[test]
    fn unknown_and_invalid_slash_warn_via_notice() {
        let mut unknown = app();
        unknown.paste_text("/foobar");
        assert!(unknown.press(Key::Enter).is_empty());
        assert!(unknown.notice().is_some_and(|n| n.contains("未知命令")));

        let mut invalid = app();
        invalid.paste_text("/skill a b");
        assert!(invalid.press(Key::Enter).is_empty());
        assert!(invalid.notice().is_some_and(|n| n.contains("用法")));
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
        app.push_system("旧内容");
        app.restore_conversation(&[*user_message("恢复的")], "sid-1".to_string());
        assert_eq!(app.items().len(), 1);
        assert!(matches!(&app.items()[0], ChatItem::User(t) if t == "恢复的"));
        assert_eq!(app.session_id(), Some("sid-1"));
    }

    // ── 会话标题（状态栏展示，替代内部 session id）────────────────────────

    #[test]
    fn first_user_message_sets_session_title() {
        let mut app = app();
        assert_eq!(app.session_label(), "无 session");
        app.set_session("sid-1".to_string());
        assert_eq!(app.session_label(), "新会话");

        app.push_user_text("实现会话命名功能\n细节补充".to_string());
        assert_eq!(app.session_label(), "实现会话命名功能");

        // 后续消息不覆盖标题
        app.push_user_text("第二条消息".to_string());
        assert_eq!(app.session_label(), "实现会话命名功能");
    }

    #[test]
    fn summary_and_skill_messages_do_not_set_title() {
        let mut app = app();
        app.set_session("sid-1".to_string());
        app.push_user_text(format!("{}压缩内容", nomic_ai::SUMMARY_PREFIX));
        assert_eq!(app.session_label(), "新会话");

        app.push_user_text("真正的问题".to_string());
        assert_eq!(app.session_label(), "真正的问题");
    }

    #[test]
    fn restore_and_new_rebuild_session_title() {
        let mut app = app();
        app.restore_conversation(&[*user_message("恢复的标题")], "sid-1".to_string());
        assert_eq!(app.session_label(), "恢复的标题");

        app.start_new_conversation();
        assert_eq!(app.session_label(), "新会话");
    }

    /// `/tree` 解析：无参命令；带参数报用法错误。
    #[test]
    fn parse_slash_tree_forms() {
        assert_eq!(parse_slash("/tree"), SlashParse::Known(SlashAction::Tree));
        assert!(matches!(
            parse_slash("/tree x"),
            SlashParse::InvalidUsage(_)
        ));
        assert!(matches!(
            parse_slash("/tree:abc"),
            SlashParse::InvalidUsage(_)
        ));
        assert_eq!(
            parse_slash("/treex"),
            SlashParse::Unknown("treex".to_string())
        );
    }

    /// `/tree` 提交 → ListTree 效果。
    #[test]
    fn tree_slash_produces_list_tree_effect() {
        let mut app = app();
        app.paste_text("/tree");
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::ListTree]));
    }

    /// `/tree` 选择器：移动跳过不可选行（工具调用条目），Enter 产出
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
        assert_eq!(app.picker().expect("picker").selected, 2);
        // 上移同样跳过
        assert!(app.press(Key::Up).is_empty());
        assert_eq!(app.picker().expect("picker").selected, 0);

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
        assert_eq!(app.picker().expect("picker").selected, 0);
    }

    /// 分支切换：以重放的消息替换聊天区，session 不变。
    #[test]
    fn restore_branch_replaces_items_keeps_session() {
        let mut app = app();
        app.set_session("sid-1".to_string());
        app.push_system("旧内容");
        app.restore_branch(&[*user_message("分支起点")]);
        assert_eq!(app.items().len(), 1);
        assert!(matches!(&app.items()[0], ChatItem::User(t) if t == "分支起点"));
        assert_eq!(app.session_id(), Some("sid-1"));
    }
}
