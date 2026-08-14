//! TUI 状态层：聊天条目、流式增量累积、输入编辑、滚动。
//!
//! 对外只暴露语义级操作——按键（[`App::press`] → [`Effect`]）、应用 agent 事件、
//! 滚动、会话与附件管理；编辑器/补全/picker/命令分发均为模块内部实现。
//! 本模块不碰终端，全部逻辑可脱离 ratatui/crossterm 单测。
//!
//! 结构：[`App`] 是「组合 + 模式路由」的薄壳，状态按关注点拆到子模块——
//! - [`chat`]：聊天区条目 + delta 累积 + 滚动（[`Chat`]）
//! - [`input`]：草稿/命令栏缓冲 + 编辑 + 补全（[`Input`]；草稿与命令
//!   栏各持一份，ADR-0020）
//! - [`queue`]：统一消息队列与 QUEUE 模式状态（[`Queue`]）
//! - [`picker`]：选择器状态（[`Picker`]）
//!
//! 子模块各自自持状态与方法集；跨模块协调（模式切换、提示语、
//! [`Effect`] 分发）由本壳完成。

mod actions;
mod chat;
mod input;
mod picker;
mod queue;
mod state;

#[cfg(test)]
mod tests;

use nomic_ai::Message;
#[cfg(test)]
use nomic_ai::StopReason;
use nomic_core::{AgentEvent, TurnMessage, estimate_context_tokens};
use nomic_prompts::PromptsError;

use chat::{assistant_error, user_text};
use input::skill_list_text;
use picker::PICKER_PAGE_SCROLL;
use queue::Queue;

use crate::picker::step_row;

pub(super) use chat::{Block, Chat, ChatItem, ToolItem, ToolStatus, skill_load_message};
#[cfg(test)]
pub(super) use input::Completion;
pub(super) use input::{CompletionCandidate, Input, MentionCompletion, SkillEntry};
pub(super) use picker::{PICKER_ROW_CAPACITY, Picker, PickerKind, PickerRow};

use crate::print::brief_args;

/// braille spinner 帧序列（运行中工具与流式指示共用）。
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 一条内建命令的静态描述。
#[derive(Debug)]
pub(super) struct Command {
    pub(super) name: &'static str,
    pub(super) aliases: &'static [&'static str],
    pub(super) summary: &'static str,
    /// 参数形式非法时的用法提示
    pub(super) usage: &'static str,
}

/// 全部内建命令（命令栏补全候选与 `help` 输出的唯一来源）。
/// 命令只在 COMMAND 模式（NORMAL `:` 打开的浮层命令栏）执行；
/// 聊天输入框（INSERT）不触发命令，`/` 开头的输入按普通 prompt 发送。
pub(super) const COMMANDS: &[Command] = &[
    Command {
        name: "help",
        aliases: &[],
        summary: "显示可用命令",
        usage: "help",
    },
    Command {
        name: "new",
        aliases: &[],
        summary: "清空上下文，开启新对话（新 session）",
        usage: "new",
    },
    Command {
        name: "resume",
        aliases: &[],
        summary: "选择并恢复历史 session（切换上下文与落库目标）",
        usage: "resume",
    },
    Command {
        name: "tree",
        aliases: &[],
        summary: "浏览会话树：选择非工具调用条目作为新分支起点（原分支保留）",
        usage: "tree",
    },
    Command {
        name: "compact",
        aliases: &[],
        summary: "压缩上下文为摘要（可带聚焦指令：compact 专注某部分）",
        usage: "compact [聚焦指令]",
    },
    Command {
        name: "continue",
        aliases: &[],
        summary: "续跑：重发最后一条消息（user 消息或 tool result）重新请求模型",
        usage: "continue",
    },
    Command {
        name: "models",
        aliases: &[],
        summary: "切换模型（跨 provider；推理模型在选择后继续选择思考级别）",
        usage: "models（选择器）或 models:<provider>/<模型id>",
    },
    Command {
        name: "skill",
        aliases: &[],
        summary: "手动载入 skill 到当前对话（skill:<name>[ args]；无参列出可用 skill）",
        usage: "skill:<name>[ args]（skill 列出可用 skill）",
    },
    Command {
        name: "image",
        aliases: &[],
        summary: "为下一条消息附加图片（可多次附加；png/jpeg/gif/webp）",
        usage: "image:<路径>（image <路径> 亦可）",
    },
    Command {
        name: "copy",
        aliases: &[],
        summary: "复制最新一条消息到剪贴板（assistant 消息取正文，不含 thinking）",
        usage: "copy",
    },
    Command {
        name: "thinking",
        aliases: &[],
        summary: "切换 thinking 内容折叠/展开显示（默认折叠）",
        usage: "thinking",
    },
    Command {
        name: "goal",
        aliases: &[],
        summary: "开关 goal 模式（默认关闭）：开启后 react loop 停止时若 todo 未全部完成，自动以 user 消息追问",
        usage: "goal",
    },
    Command {
        name: "quit",
        aliases: &["exit"],
        summary: "退出 TUI",
        usage: "quit",
    },
];

/// 命令解析结果。
#[derive(Debug, PartialEq, Eq)]
enum CommandParse {
    /// 输入仍以 `/` 开头：命令语法已无前缀（ADR-0020 修订），拒绝并提示
    SlashPrefixed,
    /// 已知命令
    Known(CommandAction),
    /// 命令存在但参数形式非法（携带用法提示）
    InvalidUsage(&'static str),
    /// 未知命令名
    Unknown(String),
}

/// `skill:<name>[ args]` 的解析结果：名称 + 可选附加上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkillInvocation {
    pub(super) name: String,
    /// 名称后首个空白起的自由文本（传给 skill 的附加上下文）
    pub(super) args: Option<String>,
}

/// 已知命令的动作。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CommandAction {
    Help,
    New,
    Resume,
    /// `tree`：浏览会话树并选择分支起点
    Tree,
    Quit,
    /// `skill`（None）列出可用 skill；`skill:<name>[ args]` 载入指定 skill
    Skill(Option<SkillInvocation>),
    /// `compact [聚焦指令]` 手动压缩上下文
    Compact(Option<String>),
    /// `continue` 续跑：重发最后一条消息（user 消息或 tool result）
    Continue,
    /// `models`（None）打开模型选择器；`models:<provider>/<id>` 直接切换
    Models(Option<String>),
    /// `image <路径>` 为下一条消息附加图片
    Image(String),
    /// `copy` 复制最新一条消息到剪贴板
    Copy,
    /// `thinking` 切换 thinking 内容折叠/展开显示
    Thinking,
    /// `goal` 开关 goal 模式（loop 停止且 todo 未完成时自动追问）
    Goal,
}

impl CommandAction {
    /// 是否为本地命令：不触碰 agent/driver 状态（不发送 driver job），
    /// 运行中（含工具执行中）可安全执行，不被工具调用阻塞。
    ///
    /// 会话命令（`new` `resume` `tree` `compact` `continue` `models`
    /// `skill:<name>`）都要经 driver 串行修改 agent 上下文，而 agent 方法
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

/// 命令名后的参数尾巴（[`command_tail`] 的返回值）。
enum CommandTail<'a> {
    /// `name` 精确命中，无参数
    Bare,
    /// `name:arg` / `name arg`
    Arg(&'a str),
}

/// 切分命令名后的参数尾巴；`namexxx`（命令名只是更长输入的前缀）
/// 返回 `None`，由调用方落入常规解析报未知命令。
fn command_tail<'a>(rest: &'a str, name: &str) -> Option<CommandTail<'a>> {
    let tail = rest.strip_prefix(name)?;
    if tail.is_empty() {
        return Some(CommandTail::Bare);
    }
    tail.strip_prefix(':')
        .or_else(|| tail.strip_prefix(' '))
        .map(CommandTail::Arg)
}

/// 解析一行命令输入。
///
/// 命令语法无前缀（ADR-0020 修订）：`quit`、`skill:jujutsu`、`compact 指令`。
/// 仍以 `/` 开头的输入返回 [`CommandParse::SlashPrefixed]`，由调用方提示
/// 新语法。参数只支持 `name:arg` 冒号形式（如 `skill:jujutsu`）；
/// `cmd extra` 视为参数形式非法（`compact`/`image`/`models` 特判除外）。
fn parse_command(input: &str) -> CommandParse {
    let rest = input.trim();
    if rest.starts_with('/') {
        return CommandParse::SlashPrefixed;
    }
    // `compact` 特判：参数是自由文本（可含空格），`compact 指令` 与
    // `compact:指令` 两种形式都接受
    if let Some(tail) = command_tail(rest, "compact") {
        let arg = match tail {
            CommandTail::Bare => None,
            CommandTail::Arg(text) => Some(text.trim()).filter(|text| !text.is_empty()),
        };
        return CommandParse::Known(CommandAction::Compact(arg.map(str::to_string)));
    }
    // `image` 特判：参数是文件路径（可含空格），`image 路径` 与
    // `image:路径` 两种形式都接受
    if let Some(tail) = command_tail(rest, "image") {
        let path = match tail {
            CommandTail::Bare => "",
            CommandTail::Arg(path) => path.trim(),
        };
        return if path.is_empty() {
            CommandParse::InvalidUsage(image_usage())
        } else {
            CommandParse::Known(CommandAction::Image(path.to_string()))
        };
    }
    // `models` 特判：参数是选择项（`<provider>/<模型id>`，不可含空格），
    // `models id` 与 `models:id` 两种形式都接受
    if let Some(tail) = command_tail(rest, "models") {
        let CommandTail::Arg(id) = tail else {
            return CommandParse::Known(CommandAction::Models(None));
        };
        let id = id.trim();
        return if id.is_empty() || id.contains(char::is_whitespace) {
            CommandParse::InvalidUsage(models_usage())
        } else {
            CommandParse::Known(CommandAction::Models(Some(id.to_string())))
        };
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
    for command in COMMANDS {
        if command.name == name || command.aliases.contains(&name) {
            let action = match command.name {
                "skill" => {
                    if junk {
                        return CommandParse::InvalidUsage(command.usage);
                    }
                    // 名称后首个空白起为附带 args（自由文本，可含空格）
                    let invocation = arg.map(|arg| {
                        let (name, args) = match arg.split_once(char::is_whitespace) {
                            Some((name, rest)) => {
                                let args = rest.trim();
                                (name, (!args.is_empty()).then(|| args.to_string()))
                            }
                            None => (arg, None),
                        };
                        SkillInvocation {
                            name: name.to_string(),
                            args,
                        }
                    });
                    CommandAction::Skill(invocation)
                }
                "help" if !junk && arg.is_none() => CommandAction::Help,
                "new" if !junk && arg.is_none() => CommandAction::New,
                "resume" if !junk && arg.is_none() => CommandAction::Resume,
                "tree" if !junk && arg.is_none() => CommandAction::Tree,
                "continue" if !junk && arg.is_none() => CommandAction::Continue,
                "copy" if !junk && arg.is_none() => CommandAction::Copy,
                "thinking" if !junk && arg.is_none() => CommandAction::Thinking,
                "goal" if !junk && arg.is_none() => CommandAction::Goal,
                "quit" if !junk && arg.is_none() => CommandAction::Quit,
                _ => return CommandParse::InvalidUsage(command.usage),
            };
            return CommandParse::Known(action);
        }
    }
    CommandParse::Unknown(name.to_string())
}

/// `help` 的输出文本。
fn help_text() -> String {
    use std::fmt::Write as _;
    // 命令只在 COMMAND 模式执行（ADR-0020）：NORMAL 下 `:` 打开命令栏
    let mut text = "可用命令（Esc 进 NORMAL 后按 : 打开命令栏，Tab 补全）：".to_string();
    for command in COMMANDS {
        let aliases = if command.aliases.is_empty() {
            String::new()
        } else {
            let list = command.aliases.join(", ");
            format!("（别名：{list}）")
        };
        let _ = write!(
            text,
            "\n  {} — {}{}",
            command.name, command.summary, aliases
        );
    }
    text.push_str(
        "\n\n排队输入（统一消息队列）：运行中 Enter 把消息排队，当前步骤完成后注入\n\
         本轮运行（未清空则持续续行）；Esc 进 NORMAL 后按 m 打开队列编辑（j/k 移动、\n\
         i 就地编辑、dd 删除、J/K 换位、o 新增、Esc 返回）。运行被取消或失败时队列\n\
         暂停保留：空闲下按 Enter 把队首作为下一轮发送。",
    );
    text.push_str(
        "\n\n长文输入：INSERT 下按 Ctrl+G 挂起 TUI，用外部编辑器（$VISUAL / $EDITOR，\n\
         缺省 vi）编辑当前草稿，保存退出后写回输入框；编辑器异常退出或内容为空时\n\
         保留原草稿。",
    );
    text.push_str("\n\n键位速查：NORMAL 下按 ? 打开快捷键帮助弹层（j/k 滚动，Esc/q/? 关闭）。");
    text
}

/// PgUp/PgDn 的滚动步长。
const PAGE_SCROLL: u16 = 10;

/// NORMAL 模式 Ctrl+D/Ctrl+U 的半页滚动步长。
const HALF_PAGE_SCROLL: u16 = 5;

/// TUI 交互模式（ADR-0021）：模式是一等状态，每个按键在当前模式
/// 只有一个语义。
///
/// COMMAND 有专门的浮层命令栏（独立缓冲，不复用输入框）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// 输入（默认）：编辑与提交 prompt；不触发命令，`/` 开头按普通文本发送
    Insert,
    /// 动作层（ADR-0021）：单字母直达——滚动、复制、队列、会话；
    /// 输入字符不进入缓冲（草稿保留）
    Normal,
    /// 命令（ADR-0020）：NORMAL `:` 进入的浮层命令栏（屏幕中上方的
    /// 单行输入框，独立缓冲，语法无 `/` 前缀）；Tab 补全、Enter 执行
    /// 命令或展开模板、Esc 放弃回 NORMAL
    Command,
    /// 队列编辑（ADR-0012，oil.nvim 式）：排队消息作为可编辑缓冲，
    /// 导航/删除/换位/就地编辑；打开期间冻结队列发送
    Queue,
    /// 键位帮助弹层（NORMAL `?` 打开）：只读浏览，j/k 滚动，
    /// Esc/q/`?` 关闭。派生态：由 `help_scroll.is_some()` 决定，
    /// 不入 `App::mode` 字段（与 Picker 同构）
    Help,
    /// 选择器打开（`resume`、`models`、`tree`），接管键位。
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
    /// Ctrl+字母（Ctrl+C/D 退出；INSERT 下 Ctrl+W/U/A/E 词级编辑；
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
    /// 提交一轮 prompt（`running` 已置位，避免提交空窗期重复提交）；
    /// 来源：用户提交、模板展开、队列 drain（ADR-0012）
    Prompt {
        text: String,
        images: Vec<nomic_ai::ImageContent>,
    },
    /// `compact` 手动压缩上下文（`running` 已置位，NORMAL `q` 可取消）
    Compact(Option<String>),
    /// `continue` 续跑：重发最后一条消息（`running` 已置位，聊天区尾部
    /// 失败/未定稿条目已随历史中的失败消息一并移除）
    Continue,
    /// 取消当前运行（NORMAL `q`、`Ctrl+C` 硬退出）
    Cancel,
    /// INSERT `Ctrl+G`：挂起 TUI，用外部编辑器（`$VISUAL`/`$EDITOR`，
    /// 缺省 `vi`，ADR-0017）编辑当前草稿；编辑器退出后由事件循环
    /// 把结果写回（[`App::apply_editor_result`]）
    OpenEditor,
    /// `resume`：列出历史 session 并打开选择器
    ListSessions,
    /// picker 确认：恢复选中的 session（加载历史 + 切换落库目标）
    Resume(String),
    /// `models`：列出当前 provider 的候选模型并打开选择器
    ListModels,
    /// `models:<provider>/<id>` 或模型选择器确认：切换为指定模型（上下文保留）；
    /// 推理模型由事件循环继续打开思考级别选择器（流程第二步）
    SwitchModel(String),
    /// 思考级别选择器确认：设置思考级别（"off" 关闭）；若有待切换模型
    /// （流程第二步）先应用模型切换
    SetReasoning(String),
    /// 思考级别选择器被 Esc 取消：放弃待切换模型，模型与级别均不变
    CancelModelSwitch,
    /// `skill`：列出可用 skill 并刷新补全快照
    ListSkills,
    /// `skill:<name>[ args]`：手动载入 skill 到当前对话
    LoadSkill(SkillInvocation),
    /// `image <路径>`：为下一条消息附加图片
    AttachImage(String),
    /// `copy`：复制最新一条消息到剪贴板
    CopyText(String),
    /// `new`：清空上下文开启新对话（新建 session）
    NewSession,
    /// `tree`：列出当前 session 的会话树并打开选择器
    ListTree,
    /// `tree` 选择器确认：以所选条目为起点创建分支（恢复该分支上下文，
    /// 后续消息以该条目为父 entry 落库）
    BranchTo(String),
}

/// TUI 应用状态：各关注点状态的组合 + 模式路由。
// 布尔字段均为相互独立的 UI 开关（运行态/退出/thinking 折叠/goal 模式），
// 两态语义清晰，无需状态机
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub(super) struct App {
    /// 聊天区：条目与滚动
    chat: Chat,
    /// 聊天输入区：草稿缓冲、编辑与附件（INSERT/QUEUE 就地编辑共用；
    /// 不触发命令，命令补全不启用）
    input: Input,
    /// 命令栏（ADR-0020 修订）：COMMAND 模式的专用缓冲（独立于草稿），
    /// 补全常驻启用；进入时清空并列出全部候选，离开即清空；
    /// 渲染为屏幕中上方的浮层单行输入框，不复用聊天输入区
    command: Input,
    /// 统一消息队列与 QUEUE 模式状态
    queue: Queue,
    /// 选择器（`resume` / `models` / `tree`，打开时接管键位）
    picker: Option<Picker>,
    /// 交互模式（ADR-0021）：只取 Insert/Normal/Command/Queue；
    /// Picker/Help 是派生态（`picker.is_some()` / `help_scroll.is_some()`
    /// 时 [`Self::mode`] 返回对应值），不入此字段
    mode: Mode,
    /// 序列键首键（QUEUE 的 `d`），等待第二键
    pending_key: Option<char>,
    /// 已提交 prompt 的历史（INSERT ↑/↓ 召回，ADR-0021）；新条目在前
    history: Vec<String>,
    /// 召回游标：`None` 表示未在召回中（输入的是当前草稿）
    history_index: Option<usize>,
    /// 召回前暂存的当前草稿（↓ 回到底时还原）
    history_stash: String,
    /// 键位帮助弹层的滚动偏移（NORMAL `?` 打开；`Some` 即打开，
    /// 0 为顶部）。派生模式 Help 的判定字段
    help_scroll: Option<u16>,
    running: bool,
    should_quit: bool,
    /// 模型展示名
    model_name: String,
    /// 当前 session id（未持久化时为 None；内部标识，不展示给用户）
    session_id: Option<String>,
    /// 上下文 token 估算（状态栏用量显示；与自动压缩同一估算口径）
    context_tokens: u64,
    /// 模型上下文窗口（0 = 规格未知，状态栏不显示占比）
    context_window: u64,
    /// 状态栏一次性提示（告警等）
    notice: Option<String>,
    /// spinner 帧序号（仅运行中由事件循环周期推进）
    spinner: usize,
    /// thinking 内容是否折叠显示（默认折叠，`thinking` 切换）
    thinking_collapsed: bool,
    /// goal 模式（默认关闭，`goal` 开关）：开启后 react loop 停止且
    /// todo 未全部完成时，由事件循环自动以 user 消息追问
    goal_mode: bool,
}

fn image_usage() -> &'static str {
    COMMANDS
        .iter()
        .find(|command| command.name == "image")
        .map_or("image:<路径>", |command| command.usage)
}

fn models_usage() -> &'static str {
    COMMANDS
        .iter()
        .find(|command| command.name == "models")
        .map_or("models:<provider>/<模型id>", |command| command.usage)
}

/// 文本的逻辑行数（空文本为 1）：草稿与队列条目共用的行数口径。
fn line_count_of(text: &str) -> u16 {
    let count = text.bytes().filter(|b| *b == b'\n').count() + 1;
    u16::try_from(count).unwrap_or(u16::MAX)
}
