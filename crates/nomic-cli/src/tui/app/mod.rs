//! TUI 状态层：聊天条目、流式增量累积、输入编辑、滚动。
//!
//! 对外只暴露语义级操作——按键（[`App::press`] → [`Effect`]）、应用 agent 事件、
//! 滚动、会话与附件管理；编辑器/补全/picker/slash 分发均为模块内部实现。
//! 本模块不碰终端，全部逻辑可脱离 ratatui/crossterm 单测。
//!
//! 结构：[`App`] 是「组合 + 模式路由」的薄壳，状态按关注点拆到子模块——
//! - [`chat`]：聊天区条目 + delta 累积 + 滚动（[`Chat`]）
//! - [`input`]：草稿/命令行缓冲 + 编辑 + 补全（[`Input`]；草稿与命令
//!   输入框各持一份，ADR-0020）
//! - [`queue`]：统一消息队列与 QUEUE 模式状态（[`Queue`]）
//! - [`picker`] / [`search`]：选择器与搜索状态（[`Picker`] / [`Search`]）
//!
//! 子模块各自自持状态与方法集；跨模块协调（模式切换、提示语、
//! [`Effect`] 分发）由本壳完成。

mod chat;
mod copymenu;
mod input;
mod picker;
mod queue;
mod search;

use nomic_ai::{Message, StopReason};
use nomic_core::{AgentEvent, SteeringMessage, estimate_context_tokens, usage_context_tokens};
use nomic_prompts::PromptsError;

use chat::{Chat, assistant_error, user_text};
use input::{Input, skill_list_text};
use picker::PICKER_PAGE_SCROLL;
use queue::Queue;
use search::Search;

pub(super) use chat::{AssistantItem, Block, ChatItem, ToolItem, ToolStatus, skill_load_message};
pub(super) use copymenu::CopyMenu;
pub(super) use input::{Completion, CompletionCandidate, SkillEntry};
pub(super) use picker::{Picker, PickerKind, PickerRow};

use crate::print::brief_args;

/// braille spinner 帧序列（运行中工具与流式指示共用）。
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// 一条 slash 命令的静态描述。
#[derive(Debug)]
pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) aliases: &'static [&'static str],
    pub(super) summary: &'static str,
    /// 参数形式非法时的用法提示
    pub(super) usage: &'static str,
}

/// 全部 slash 命令（命令行补全候选与 `/help` 输出的唯一来源）。
/// 命令只在 COMMAND 模式（NORMAL `:` 打开的专门命令输入框）执行；
/// 聊天输入框（INSERT）不再触发命令，`/` 开头的输入按普通 prompt 发送。
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
        summary: "手动载入 skill 到当前对话（/skill:<name>[ args]；无参列出可用 skill）",
        usage: "/skill:<name>[ args]（/skill 列出可用 skill）",
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

/// `/skill:<name>[ args]` 的解析结果：名称 + 可选附加上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SkillInvocation {
    pub(super) name: String,
    /// 名称后首个空白起的自由文本（传给 skill 的附加上下文）
    pub(super) args: Option<String>,
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
    /// `/skill`（None）列出可用 skill；`/skill:<name>[ args]` 载入指定 skill
    Skill(Option<SkillInvocation>),
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
                    if junk {
                        return SlashParse::InvalidUsage(command.usage);
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
                    SlashAction::Skill(invocation)
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
    // 命令只在 COMMAND 模式执行（ADR-0020）：NORMAL 下 `:` 打开命令输入框
    let mut text = "可用命令（Esc 进 NORMAL 后按 : 打开命令行，Tab 补全）：".to_string();
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
/// SEARCH 复用输入框显示搜索串；COMMAND 有专门的命令输入框（独立缓冲）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// 输入（默认）：编辑与提交 prompt；不触发命令，`/` 开头按普通文本发送
    Insert,
    /// 动作层（ADR-0021）：单字母直达——滚动、跳转、复制、队列、会话；
    /// 输入字符不进入缓冲（草稿保留）
    Normal,
    /// 命令（ADR-0020）：NORMAL `:` 进入的专门命令输入框（独立缓冲，预填
    /// `/`）；Tab 补全、Enter 执行命令或展开模板、Esc 放弃回 NORMAL
    Command,
    /// 搜索：输入框复用为搜索框（增量命中），Enter/Esc 回 NORMAL
    Search,
    /// 队列编辑（ADR-0012，oil.nvim 式）：排队消息作为可编辑缓冲，
    /// 导航/删除/换位/就地编辑；打开期间冻结队列发送
    Queue,
    /// 键位帮助弹层（NORMAL `?` 打开）：只读浏览，j/k 滚动，
    /// Esc/q/`?` 关闭。派生态：由 `help_scroll.is_some()` 决定，
    /// 不入 `App::mode` 字段（与 Picker 同构）
    Help,
    /// 复制菜单（NORMAL `y` 打开，ADR-0021）：消息与代码块快照列表，
    /// Enter/数字键复制、Esc/q 关闭。派生态：由 `copy_menu.is_some()` 决定
    CopyMenu,
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
    /// 提交一轮 prompt（`running` 已置位，避免提交空窗期重复提交）；
    /// 来源：用户提交、模板展开、队列 drain（ADR-0012）
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
    /// INSERT `Ctrl+G`：挂起 TUI，用外部编辑器（`$VISUAL`/`$EDITOR`，
    /// 缺省 `vi`，ADR-0017）编辑当前草稿；编辑器退出后由事件循环
    /// 把结果写回（[`App::apply_editor_result`]）
    OpenEditor,
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
    /// `/skill:<name>[ args]`：手动载入 skill 到当前对话
    LoadSkill(SkillInvocation),
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

/// TUI 应用状态：各关注点状态的组合 + 模式路由。
// 布尔字段均为相互独立的 UI 开关（运行态/退出/thinking 折叠/goal 模式），
// 两态语义清晰，无需状态机
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub(super) struct App {
    /// 聊天区：条目、消息游标与滚动
    chat: Chat,
    /// 聊天输入区：草稿缓冲、编辑与附件（INSERT/QUEUE 就地编辑共用；
    /// 不触发命令，slash 补全不启用）
    input: Input,
    /// 命令输入框（ADR-0020）：COMMAND 模式的专用缓冲（独立于草稿），
    /// slash 补全常驻启用；进入时清空并预填 `/`，离开即清空
    command: Input,
    /// 统一消息队列与 QUEUE 模式状态
    queue: Queue,
    /// 搜索状态（NORMAL `/` 进入 SEARCH）
    search: Search,
    /// 选择器（`/resume` / `/models` / `/tree`，打开时接管键位）
    picker: Option<Picker>,
    /// 复制菜单（NORMAL `y` 打开；`Some` 即打开）
    copy_menu: Option<CopyMenu>,
    /// 交互模式（ADR-0021）：只取 Insert/Normal/Search/Queue；
    /// Picker/Help/CopyMenu 是派生态（`picker.is_some()` /
    /// `help_scroll.is_some()` / `copy_menu.is_some()` 时 [`Self::mode`]
    /// 返回对应值），不入此字段
    mode: Mode,
    /// 序列键首键（QUEUE/HELP 的 `g`/`d`），等待第二键
    pending_key: Option<char>,
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
    /// thinking 内容是否折叠显示（默认折叠，`/thinking` 切换）
    thinking_collapsed: bool,
    /// goal 模式（默认关闭，`/goal` 开关）：开启后 react loop 停止且
    /// todo 未全部完成时，由事件循环自动以 user 消息追问
    goal_mode: bool,
}

impl App {
    pub(super) fn new(model_name: String, session_id: Option<String>, context_window: u64) -> Self {
        let mut input = Input::new();
        // 草稿不承载命令（ADR-0020）：slash 补全只属于命令输入框
        input.set_completion_enabled(false);
        Self {
            chat: Chat::default(),
            input,
            command: Input::new(),
            queue: Queue::default(),
            search: Search::default(),
            picker: None,
            copy_menu: None,
            mode: Mode::Insert,
            pending_key: None,
            help_scroll: None,
            running: false,
            should_quit: false,
            model_name,
            session_id,
            context_tokens: 0,
            context_window,
            notice: None,
            spinner: 0,
            thinking_collapsed: true,
            goal_mode: false,
        }
    }

    // ── 子模块访问（渲染与事件循环的读/回写通道） ────────────────────────────

    /// 聊天区状态（条目、滚动、渲染回写）。
    pub(super) const fn chat(&self) -> &Chat {
        &self.chat
    }

    /// 聊天区状态（可变）：渲染回写滚动边界/条目行号、滚动与系统提示用。
    pub(super) const fn chat_mut(&mut self) -> &mut Chat {
        &mut self.chat
    }

    /// 输入区状态（草稿、补全、附件）。
    pub(super) const fn input(&self) -> &Input {
        &self.input
    }

    /// 输入区状态（可变）：附件暂存用。
    pub(super) const fn input_mut(&mut self) -> &mut Input {
        &mut self.input
    }

    /// 命令输入框状态（COMMAND 模式渲染用）。
    pub(super) const fn command(&self) -> &Input {
        &self.command
    }

    /// 命令输入框状态（可变）：skill/template 补全快照用。
    pub(super) const fn command_mut(&mut self) -> &mut Input {
        &mut self.command
    }

    /// 队列状态（条数、条目视图、QUEUE 模式游标）。
    pub(super) const fn queue(&self) -> &Queue {
        &self.queue
    }

    /// 搜索状态（查询串、命中数、高亮词）。
    pub(super) const fn search(&self) -> &Search {
        &self.search
    }

    // ── 事件与历史 ──────────────────────────────────────────────────────────

    /// 把 resume 恢复的历史消息渲染为聊天条目。
    pub(super) fn load_history(&mut self, messages: &[Message]) {
        self.context_tokens = estimate_context_tokens(messages);
        self.chat.load_history(messages);
    }

    /// 消费一个 agent 事件，更新状态。
    pub(super) fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart => self.running = true,
            AgentEvent::MessageStart(message) => match message.as_ref() {
                Message::User(user) => {
                    self.chat.push_user_text(user_text(&user.content));
                }
                Message::Assistant(_) => self.chat.start_assistant(),
                Message::ToolResult(_) => {}
            },
            AgentEvent::MessageUpdate(delta) => self.chat.apply_delta(delta),
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
                    self.chat.finalize_assistant(assistant_error(
                        assistant.stop_reason,
                        assistant.error_message.as_deref(),
                    ));
                }
            }
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                self.chat.push_tool(ToolItem {
                    id: tool_call_id.clone(),
                    name: tool_name.clone(),
                    args: brief_args(tool_name, args),
                    status: ToolStatus::Running,
                    detail: Vec::new(),
                    collapsed: false,
                });
            }
            AgentEvent::ToolExecutionUpdate {
                tool_call_id,
                partial,
                ..
            } => {
                self.chat.update_tool_detail(tool_call_id, &partial.content);
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => {
                self.chat
                    .finish_tool(tool_call_id, *is_error, &result.content);
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
                self.chat.push_system(format!(
                    "上下文已压缩：约 {tokens_before} tokens → 摘要 + {kept_count} 条近期消息。"
                ));
            }
            AgentEvent::AgentEnd { .. } | AgentEvent::TurnStart | AgentEvent::TurnEnd { .. } => {}
        }
    }

    // ── 按键（语义分发） ────────────────────────────────────────────────────

    /// 当前交互模式（渲染光标/徽标与外部查询用）：picker/帮助弹层/
    /// 复制菜单打开时派生为对应模式，否则为字段值（Insert/Normal）。
    pub(super) const fn mode(&self) -> Mode {
        if self.picker.is_some() {
            Mode::Picker
        } else if self.help_scroll.is_some() {
            Mode::Help
        } else if self.copy_menu.is_some() {
            Mode::CopyMenu
        } else {
            self.mode
        }
    }

    /// 消费一个按键，返回需要事件循环接线执行的语义效果。
    /// 按交互模式分发（ADR-0011）：picker/补全/命令的路由全部
    /// 在此内部完成。
    pub(super) fn press(&mut self, key: Key) -> Vec<Effect> {
        match self.mode() {
            // 选择器打开时接管键位（命令仅在空闲时可提交，
            // 此时 agent 必空闲，无运行可取消）
            Mode::Picker => self.press_picker(key),
            Mode::Help => self.press_help(key),
            Mode::CopyMenu => self.press_copy_menu(key),
            Mode::Search => self.press_search(key),
            Mode::Normal => self.press_normal(key),
            Mode::Insert => self.press_insert(key),
            Mode::Command => self.press_command(key),
            Mode::Queue => self.press_queue(key),
        }
    }

    /// INSERT 模式键位：编辑与提交 prompt。命令不在此触发（ADR-0020）：
    /// `/` 开头的输入按普通 prompt 发送，命令走 COMMAND 模式。
    fn press_insert(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Ctrl('c' | 'd') => {
                if self.running {
                    return vec![Effect::Cancel];
                }
                self.should_quit = true;
            }
            // Esc 一律是模式切换（ADR-0011），不再取消运行；取消由 Ctrl+C 承担
            Key::Esc => self.enter_normal(),
            // Ctrl+G：外部编辑器编辑当前草稿（长文/多行场景；编辑器持有
            // 草稿副本，保存退出后整体写回，放弃则原样保留）
            Key::Ctrl('g') => return vec![Effect::OpenEditor],
            Key::Enter => return self.press_enter(),
            other => Self::edit_key(&mut self.input, &mut self.chat, other),
        }
        Vec::new()
    }

    /// 缓冲编辑键（INSERT、COMMAND 与 QUEUE 就地编辑共用）：字符输入、
    /// 删除、光标移动、换行与聊天区滚动；提交、补全与模式切换由调用方
    /// 各自处理。
    fn edit_key(input: &mut Input, chat: &mut Chat, key: Key) {
        match key {
            Key::Ctrl('w') => input.delete_word_back(),
            Key::Ctrl('u') => input.delete_to_line_start(),
            Key::Ctrl('a') => input.cursor_line_home(),
            Key::Ctrl('e') => input.cursor_line_end(),
            Key::Alt('b') => input.cursor_word_left(),
            Key::Alt('f') => input.cursor_word_right(),
            Key::Newline => input.insert_newline(),
            Key::Backspace => input.backspace(),
            Key::Left => input.cursor_left(),
            Key::Right => input.cursor_right(),
            Key::Home => input.cursor_home(),
            Key::End => input.cursor_end(),
            // 补全弹层可见时 ↑/↓ 移动选中项，否则滚动聊天区
            Key::Up => Self::edit_vertical(input, chat, -1),
            Key::Down => Self::edit_vertical(input, chat, 1),
            Key::PageUp => chat.scroll_up(PAGE_SCROLL),
            Key::PageDown => chat.scroll_down(PAGE_SCROLL),
            Key::Char(c) => input.insert_char(c),
            _ => {}
        }
    }

    /// 编辑态的 ↑/↓：补全弹层可见时移动选中项，否则滚动聊天区。
    const fn edit_vertical(input: &mut Input, chat: &mut Chat, delta: isize) {
        if input.completion().is_some() {
            input.completion_select(delta);
        } else if delta < 0 {
            chat.scroll_up(1);
        } else {
            chat.scroll_down(1);
        }
    }

    /// COMMAND 模式键位（ADR-0020）：专门的命令输入框（NORMAL `:` 进入，
    /// 独立缓冲预填 `/`）。编辑键与 INSERT 一致；Tab 补全，Enter 执行
    /// 命令（或展开模板），Esc 退回栈：关补全弹层 → 放弃回 NORMAL。
    fn press_command(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Ctrl('c' | 'd') => {
                if self.running {
                    return vec![Effect::Cancel];
                }
                self.should_quit = true;
            }
            Key::Esc => {
                if !self.command.dismiss_completion() {
                    self.leave_command();
                }
            }
            Key::Tab => self.command.tab_complete(),
            Key::Enter => return self.command_enter(),
            other => Self::edit_key(&mut self.command, &mut self.chat, other),
        }
        Vec::new()
    }

    /// NORMAL `:`：进入 COMMAND（专门的命令输入框）：清空缓冲并预填
    /// `/`（补全弹层随之列出全部命令）。草稿在独立缓冲，不受影响。
    fn enter_command(&mut self) {
        self.mode = Mode::Command;
        self.pending_key = None;
        self.command.set_text(String::new());
        self.command.insert_char('/');
    }

    /// 离开 COMMAND 回 NORMAL：清空命令缓冲（无论已执行还是放弃）。
    fn leave_command(&mut self) {
        self.mode = Mode::Normal;
        self.pending_key = None;
        self.command.set_text(String::new());
    }

    /// COMMAND 的 Enter：空命令行（仅预填的 `/`）无声返回 NORMAL；
    /// 补全弹层未精确匹配时先填入候选；其余按命令分发——被拒绝（参数
    /// 非法、未知命令、运行中会话命令）时留在 COMMAND 供修正，受理后
    /// 回 NORMAL（vim `:` 执行完回 normal 的同一口径）。
    fn command_enter(&mut self) -> Vec<Effect> {
        let text = self.command.text().trim().to_string();
        if text.is_empty() || text == "/" {
            // 空命令行：等同 Esc，无声返回 NORMAL
            self.leave_command();
            return Vec::new();
        }
        if self.command.accept_completion_on_enter() {
            // 已填入补全候选；再次 Enter 提交
            return Vec::new();
        }
        let Some(effects) = self.dispatch_command(&text) else {
            return Vec::new();
        };
        self.leave_command();
        effects
    }

    /// 命令行提交的分发：slash 命令 / prompt template 展开。返回 `None`
    /// 表示被拒绝（notice 已置，调用方留在 COMMAND 供修正）；`Some`
    /// 表示已受理（效果可为空，如 `/help` 就地输出）。
    ///
    /// 运行中的口径与 INSERT 提交一致（ADR-0014）：本地命令照常执行；
    /// 模板展开的 prompt 排入统一消息队列；会话命令（经 driver 修改
    /// agent 上下文）仍须等本轮结束，拒绝并保留输入。
    fn dispatch_command(&mut self, text: &str) -> Option<Vec<Effect>> {
        match parse_slash(text) {
            SlashParse::NotCommand => {
                // 缓冲预填 `/`，只有用户删掉前缀才会落到这里
                self.notice = Some("命令以 / 开头（/help 查看可用命令）".to_string());
                None
            }
            SlashParse::Known(action) => {
                if self.running && !action.is_local() {
                    self.notice = Some(
                        "运行中：会话命令（/compact、/retry、/models 等）须等本轮结束".to_string(),
                    );
                    return None;
                }
                self.notice = None;
                Some(self.execute_slash(action))
            }
            SlashParse::InvalidUsage(usage) => {
                self.notice = Some(format!("参数形式不对，用法：{usage}"));
                None
            }
            SlashParse::Unknown(name) => {
                match nomic_prompts::expand_invocation(self.command.templates(), text) {
                    Ok(Some(expanded)) => {
                        if self.running {
                            Some(self.enqueue(expanded))
                        } else {
                            let images = self.input.take_attachments();
                            // 与普通 prompt 同一口径：先置 running 避免提交空窗期重复提交
                            self.running = true;
                            self.notice = None;
                            Some(vec![Effect::Prompt {
                                text: expanded,
                                images,
                            }])
                        }
                    }
                    Err(PromptsError::UnterminatedQuote { .. }) => {
                        self.notice = Some("参数形式不对：引号未闭合".to_string());
                        None
                    }
                    _ => {
                        self.notice = Some(format!("未知命令 /{name}，输入 /help 查看可用命令"));
                        None
                    }
                }
            }
        }
    }

    /// NORMAL 模式键位（ADR-0021）：单字母动作层——less 式滚动（j/k、
    /// d/u 半页、g/G 顶底）、`[`/`]` 消息跳转、`/` 搜索、`y` 复制菜单、
    /// `m` 队列、`r` 重试、`e` 编辑器、`q` 退出；输入字符不进入缓冲
    ///（草稿保留，`i`/`a`/`Enter` 回到 INSERT 继续编辑）。
    fn press_normal(&mut self, key: Key) -> Vec<Effect> {
        if let Some(effects) = self.normal_exit(key) {
            return effects;
        }
        match key {
            // g/G：到顶/回底（less 惯例；渲染时经 clamp_scroll 钳到上限）
            Key::Char('g') => {
                self.chat.scroll_up(u16::MAX);
                self.chat.move_cursor_to_first_message();
            }
            Key::Char('G') => {
                self.chat.scroll_to_bottom();
                self.chat.move_cursor_to_last_message();
            }
            // d/u：半页下/上（less 惯例）
            Key::Char('d') => self.chat.scroll_down(HALF_PAGE_SCROLL),
            Key::Char('u') => self.chat.scroll_up(HALF_PAGE_SCROLL),
            // [/]：上一条/下一条对话消息；{/}：上一个/下一个工具调用
            Key::Char('[') => self.chat.step_cursor(-1, ChatItem::is_message),
            Key::Char(']') => self.chat.step_cursor(1, ChatItem::is_message),
            Key::Char('{') => self.chat.step_cursor(-1, ChatItem::is_tool),
            Key::Char('}') => self.chat.step_cursor(1, ChatItem::is_tool),
            // `/` 进入搜索（输入框复用为搜索框；保留上次查询可编辑）
            Key::Char('/') => self.mode = Mode::Search,
            // n/N：在搜索命中条目间循环跳转
            Key::Char('n') => self.search_jump(1),
            Key::Char('N') => self.search_jump(-1),
            // y：复制菜单（消息与代码块快照）；Y：直接复制最新一条（等价 /copy）
            Key::Char('y') => self.open_copy_menu(),
            Key::Char('Y') => return self.copy_latest(),
            // Space：折叠/展开游标条目（assistant/工具；user/system 不可折叠）
            Key::Char(' ') => {
                if !self.chat.toggle_collapsed() {
                    self.notice = Some("该条目不可折叠".to_string());
                }
            }
            // m：队列编辑 overlay（oil.nvim 式，ADR-0014）
            Key::Char('m') => self.enter_queue(),
            // r：重试最近失败的一轮（与 /retry 同一口径；运行中拒绝）
            Key::Char('r') => return self.retry_last(),
            // e：外部编辑器编辑草稿（与 INSERT Ctrl+G 同一效果）
            Key::Char('e') => return vec![Effect::OpenEditor],
            // `?` 打开键位帮助弹层（只读；Esc/q/`?` 关闭）
            Key::Char('?') => return self.open_help(),
            // q：退出（运行中先中断再退出）
            Key::Char('q') | Key::Ctrl('c') => return self.quit(),
            Key::Char('k') | Key::Up => self.chat.scroll_up(1),
            Key::Char('j') | Key::Down => self.chat.scroll_down(1),
            Key::PageUp => self.chat.scroll_up(PAGE_SCROLL),
            Key::PageDown => self.chat.scroll_down(PAGE_SCROLL),
            // 其余按键（含普通字符）忽略：不污染输入缓冲
            _ => {}
        }
        Vec::new()
    }

    /// 退出 TUI（NORMAL `q`/`Ctrl+C`）：运行中先中断本轮再退出。
    fn quit(&mut self) -> Vec<Effect> {
        self.should_quit = true;
        if self.running {
            return vec![Effect::Cancel];
        }
        Vec::new()
    }

    /// NORMAL `r`：重试最近失败的一轮（与 `/retry` 同一口径）；
    /// 运行中拒绝并提示。
    fn retry_last(&mut self) -> Vec<Effect> {
        if self.running {
            self.notice = Some("运行中：等本轮结束后再重试".to_string());
            return Vec::new();
        }
        self.chat.pop_trailing_failed_assistant();
        self.running = true;
        self.notice = None;
        vec![Effect::Retry]
    }

    /// NORMAL 的「离开动作层」键位：`i`/`a` 回 INSERT（光标原位），
    /// `Enter`/`A` 到输入末尾，`I` 到当前行首，`:` 进入 COMMAND 命令
    /// 输入框（ADR-0020）；`Esc` 逐层退回——运行中先中断运行（留在
    /// NORMAL），空闲回 INSERT。返回 `Some` 表示已处理。
    fn normal_exit(&mut self, key: Key) -> Option<Vec<Effect>> {
        match key {
            // Esc 逐层退回（ADR-0021）：运行中优先中断运行
            Key::Esc => {
                if self.running {
                    return Some(vec![Effect::Cancel]);
                }
                self.leave_normal();
            }
            // i/a 回到光标原处继续编辑
            Key::Char('i' | 'a') => self.leave_normal(),
            // Enter/A 回 INSERT 并把光标置于输入末尾（ADR-0011）；
            // I 回 INSERT 到当前逻辑行首
            Key::Enter | Key::Char('A') => {
                self.leave_normal();
                self.input.cursor_end();
            }
            Key::Char('I') => {
                self.leave_normal();
                self.input.cursor_line_home();
            }
            // `:` 进入专门的命令输入框（ADR-0020）：独立缓冲预填 `/`，
            // 草稿不受影响（补全弹层随之列出全部命令）
            Key::Char(':') => self.enter_command(),
            _ => return None,
        }
        Some(Vec::new())
    }

    /// 复制菜单键位（NORMAL `y` 打开，ADR-0021）：j/k/g/G 导航，
    /// Enter 复制选中行后关闭，数字键 `1`-`9` 直达复制对应行，
    /// Esc/q 关闭。
    fn press_copy_menu(&mut self, key: Key) -> Vec<Effect> {
        let Some(menu) = &mut self.copy_menu else {
            return Vec::new();
        };
        match key {
            Key::Char('j') | Key::Down => menu.select(1),
            Key::Char('k') | Key::Up => menu.select(-1),
            Key::Char('g') => menu.jump_first(),
            Key::Char('G') => menu.jump_last(),
            Key::Enter => {
                let text = menu.selected_text();
                self.copy_menu = None;
                return vec![Effect::CopyText(text)];
            }
            Key::Char(c @ '1'..='9') => {
                let index = (c.to_digit(10).unwrap_or(1) - 1) as usize;
                if let Some(text) = menu.select_index(index) {
                    self.copy_menu = None;
                    return vec![Effect::CopyText(text)];
                }
            }
            Key::Esc | Key::Char('q') => self.copy_menu = None,
            Key::Ctrl('c') => self.should_quit = true,
            _ => {}
        }
        Vec::new()
    }

    /// NORMAL `y`：打开复制菜单（聊天条目快照；无可复制内容时提示）。
    fn open_copy_menu(&mut self) {
        match CopyMenu::build(self.chat.items(), self.chat.cursor()) {
            Some(menu) => self.copy_menu = Some(menu),
            None => self.notice = Some("没有可复制的消息".to_string()),
        }
    }

    /// SEARCH 模式键位：输入即搜（增量跳转第一个命中），Enter 保留命中
    /// 回 NORMAL（n/N 可继续跳），Esc 清空搜索回 NORMAL。
    fn press_search(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Char(c) => {
                self.search.push_char(c);
                self.refresh_search();
            }
            Key::Backspace => {
                self.search.pop_char();
                self.refresh_search();
            }
            Key::Enter => {
                self.mode = Mode::Normal;
                let count = self.search.match_count();
                self.notice = Some(if count == 0 {
                    "没有搜索命中".to_string()
                } else {
                    format!("{count} 处命中 · n/N 跳转")
                });
            }
            Key::Esc => {
                self.mode = Mode::Normal;
                self.search.clear();
            }
            Key::Ctrl('c') => self.should_quit = true,
            _ => {}
        }
        Vec::new()
    }

    /// 重算搜索命中（输入即搜）：游标跳到当前位置之后（含）的第一个
    /// 命中（循环），无命中保持游标。
    fn refresh_search(&mut self) {
        if let Some(target) = self.search.refresh(self.chat.items(), self.chat.cursor()) {
            self.chat.focus_item(target);
        }
    }

    /// NORMAL `n`/`N`：在搜索命中条目间循环跳转。
    fn search_jump(&mut self, direction: isize) {
        let Some((index, pos)) = self.search.jump(direction, self.chat.cursor().unwrap_or(0))
        else {
            self.notice = Some("没有搜索命中（NORMAL 下 / 开始搜索）".to_string());
            return;
        };
        self.chat.focus_item(index);
        self.notice = Some(format!("命中 {}/{}", pos + 1, self.search.match_count()));
    }

    /// 进入 NORMAL：草稿保留；消息游标定位到最新一条对话消息。
    fn enter_normal(&mut self) {
        self.mode = Mode::Normal;
        self.pending_key = None;
        self.chat.move_cursor_to_last_message();
    }

    /// 离开 NORMAL 回 INSERT：清掉序列键 pending，避免残留的首键
    /// 在下次进入 NORMAL 时被误当第二键。
    const fn leave_normal(&mut self) {
        self.mode = Mode::Insert;
        self.pending_key = None;
    }

    /// 消息游标（渲染 gutter 高亮用）；浏览类模式（NORMAL/COMMAND/
    /// SEARCH/COPYMENU）下返回。
    pub(super) fn chat_cursor(&self) -> Option<usize> {
        matches!(
            self.mode(),
            Mode::Normal | Mode::Command | Mode::Search | Mode::CopyMenu
        )
        .then_some(self.chat.cursor())
        .flatten()
    }

    /// 复制最新一条消息到剪贴板（`/copy` 与 NORMAL `Y` 共用）。
    fn copy_latest(&mut self) -> Vec<Effect> {
        if let Some(text) = self.chat.latest_message_text() {
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
                if self.picker.as_mut().is_some_and(Picker::clear_filter) {
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
                self.picker = None;
                if abort_switch {
                    return vec![Effect::CancelModelSwitch];
                }
            }
            Key::Backspace => {
                if let Some(picker) = &mut self.picker {
                    picker.pop_filter();
                }
            }
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
                    picker.push_filter_char(c);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// 移动 picker 选中项（picker 打开时）。
    fn picker_select(&mut self, delta: isize) {
        if let Some(picker) = &mut self.picker {
            picker.select(delta);
        }
    }

    /// 跳转 picker 选中到可见行的 `pos`（picker 打开时）。
    fn picker_jump(&mut self, pos: usize, direction: isize) {
        if let Some(picker) = &mut self.picker {
            picker.jump(pos, direction);
        }
    }

    /// INSERT 的 Enter：取出草稿提交 prompt（运行中的口径见
    /// [`Self::press_enter_running`]）。命令不在此触发（ADR-0020）：
    /// `/` 开头的草稿同样按普通 prompt 发送，命令走 COMMAND 模式
    ///（NORMAL `:` 打开命令输入框）。
    fn press_enter(&mut self) -> Vec<Effect> {
        if self.running {
            return self.press_enter_running();
        }
        let Some(text) = self.input.take_input() else {
            if self.input.has_attachments() {
                self.notice = Some("已附加图片，输入文本后 Enter 一起发送".to_string());
            } else if let Some(effect) = self.drain_queue() {
                // 空闲 + 空草稿 + 队列有暂停的排队消息：Enter 直接发送下一条
                return vec![effect];
            }
            return Vec::new();
        };
        let images = self.input.take_attachments();
        // AgentStart 事件也会置位；先置避免提交空窗期重复提交
        self.running = true;
        self.notice = None;
        vec![Effect::Prompt { text, images }]
    }

    /// 运行中（含工具执行中）的 INSERT Enter：普通输入**排队**——入
    /// 统一消息队列（ADR-0014，当前 turn 的工具调用执行完后注入本轮
    /// 运行）；Esc→NORMAL→m 进 QUEUE 模式编辑队列。运行中执行命令走
    /// COMMAND 模式（本地命令照常，会话命令仍须等本轮结束）。
    fn press_enter_running(&mut self) -> Vec<Effect> {
        let Some(text) = self.input.take_input() else {
            if self.input.has_attachments() {
                self.notice = Some("已附加图片，输入文本后 Enter 一起排队".to_string());
            }
            return Vec::new();
        };
        self.enqueue(text)
    }

    // ── 排队输入与 QUEUE 模式（ADR-0014）───────────────────────────

    /// 入队（ADR-0014，统一消息队列）：随暂存附件一起入队，当前 turn
    /// 的工具调用执行完后由 core 在 turn 边界注入本轮运行（run 异常
    /// 结束时保留，恢复后作为下一轮 prompt）；Esc→NORMAL→m 进 QUEUE
    /// 模式可编辑（编辑期间冻结注入）。
    fn enqueue(&mut self, text: String) -> Vec<Effect> {
        let images = self.input.take_attachments();
        self.queue.push(SteeringMessage { text, images });
        self.notice = Some(format!(
            "已排队（第 {} 条），当前步骤完成后注入本轮 · Esc→m 编辑队列",
            self.queue.len()
        ));
        Vec::new()
    }

    /// 取出下一条待发消息（run 异常结束后恢复路径；正常结束的 run
    /// 其队列已被 core 排空）：队列非空且 QUEUE 模式未打开时返回提交
    /// 效果（`running` 已置位，与用户手动提交同一口径）；QUEUE 模式
    /// 打开期间冻结发送，空队列返回 `None`。
    pub(super) fn drain_queue(&mut self) -> Option<Effect> {
        if self.mode == Mode::Queue {
            return None;
        }
        let queued = self.queue.pop_front()?;
        self.running = true;
        self.notice = None;
        Some(Effect::Prompt {
            text: queued.text,
            images: queued.images,
        })
    }

    /// QUEUE 模式键位：导航子状态移动/删除/换位/新增，`i`/`Enter` 就地
    /// 编辑；编辑子状态复用缓冲编辑键，Enter/Esc 保存回队列。
    fn press_queue(&mut self, key: Key) -> Vec<Effect> {
        if self.queue.is_editing() {
            return self.press_queue_edit(key);
        }
        // 序列键第二键（gg 到队首、dd 删除）；不匹配照常分发
        if let Some(pending) = self.pending_key.take()
            && let Some(effects) = self.queue_sequence(pending, key)
        {
            return effects;
        }
        match key {
            Key::Char('g') => self.pending_key = Some('g'),
            Key::Char('d') => self.pending_key = Some('d'),
            Key::Char('j') | Key::Down => self.queue.move_cursor(1),
            Key::Char('k') | Key::Up => self.queue.move_cursor(-1),
            Key::Char('G') => self.queue.jump_to_last(),
            Key::Char('x') => self.queue_delete(),
            Key::Char('J') => self.queue.swap(1),
            Key::Char('K') => self.queue.swap(-1),
            Key::Char('i' | 'a') | Key::Enter => self.queue_begin_edit(),
            Key::Char('o') => self.queue_insert_slot(1),
            Key::Char('O') => self.queue_insert_slot(0),
            Key::Esc => return self.leave_queue(),
            Key::Ctrl('c') => {
                if self.running {
                    return vec![Effect::Cancel];
                }
                self.should_quit = true;
            }
            Key::PageUp => self.chat.scroll_up(PAGE_SCROLL),
            Key::PageDown => self.chat.scroll_down(PAGE_SCROLL),
            _ => {}
        }
        Vec::new()
    }

    /// QUEUE 的序列键第二键：`gg` 到队首、`dd` 删除游标条目。
    /// 返回 `Some` 表示已处理。
    fn queue_sequence(&mut self, pending: char, key: Key) -> Option<Vec<Effect>> {
        match (pending, key) {
            ('g', Key::Char('g')) => self.queue.jump_to_first(),
            ('d', Key::Char('d')) => self.queue_delete(),
            _ => return None,
        }
        Some(Vec::new())
    }

    /// QUEUE 编辑子状态键位：Enter/Esc 保存（vim 保存即应用），
    /// 其余按键与 INSERT 的缓冲编辑一致（补全在 QUEUE 下不启用）。
    fn press_queue_edit(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Enter | Key::Esc => self.queue_save_edit(),
            Key::Ctrl('c') => {
                if self.running {
                    return vec![Effect::Cancel];
                }
                self.should_quit = true;
            }
            other => Self::edit_key(&mut self.input, &mut self.chat, other),
        }
        Vec::new()
    }

    /// NORMAL `?`：打开键位帮助弹层（滚动置顶；Esc/q/`?` 关闭）。
    const fn open_help(&mut self) -> Vec<Effect> {
        self.help_scroll = Some(0);
        Vec::new()
    }

    /// HELP 弹层键位（NORMAL `?` 打开）：只读浏览，j/k 等滚动、
    /// gg/G 到顶/底；Esc/q/`?` 关闭回到底层模式（mode 字段未动，
    /// 天然回到打开前的 NORMAL）。其余按键忽略，不污染输入缓冲。
    fn press_help(&mut self, key: Key) -> Vec<Effect> {
        // 序列键第二键（gg 到顶）；不匹配照常分发
        if self.pending_key.take() == Some('g') && key == Key::Char('g') {
            self.help_scroll = Some(0);
            return Vec::new();
        }
        match key {
            Key::Esc | Key::Char('q' | '?') => self.help_scroll = None,
            Key::Char('g') => self.pending_key = Some('g'),
            // G 到底：渲染时经 clamp_help_scroll 钳到实际上限
            Key::Char('G') => self.help_scroll = Some(u16::MAX),
            Key::Char('j') | Key::Down => self.help_scroll_by(1),
            Key::Char('k') | Key::Up => self.help_scroll_by(-1),
            Key::Ctrl('d') => self.help_scroll_by(i32::from(HALF_PAGE_SCROLL)),
            Key::Ctrl('u') => self.help_scroll_by(-i32::from(HALF_PAGE_SCROLL)),
            Key::PageDown => self.help_scroll_by(i32::from(PAGE_SCROLL)),
            Key::PageUp => self.help_scroll_by(-i32::from(PAGE_SCROLL)),
            Key::Ctrl('c') => {
                if self.running {
                    return vec![Effect::Cancel];
                }
                self.should_quit = true;
            }
            _ => {}
        }
        Vec::new()
    }

    /// HELP 弹层滚动（下正上负，钳制不循环；上限由渲染回写钳制）。
    fn help_scroll_by(&mut self, delta: i32) {
        let Some(scroll) = self.help_scroll else {
            return;
        };
        self.help_scroll = Some(if delta < 0 {
            scroll.saturating_sub(u16::try_from(delta.unsigned_abs()).unwrap_or(u16::MAX))
        } else {
            scroll.saturating_add(u16::try_from(delta).unwrap_or(u16::MAX))
        });
    }

    /// QUEUE `dd`/`x`：删除游标条目；队列清空时退出 QUEUE 回 NORMAL。
    fn queue_delete(&mut self) {
        if self.queue.delete() {
            self.mode = Mode::Normal;
            self.notice = Some("队列已清空".to_string());
        }
    }

    /// QUEUE `i`/`a`/Enter：开始就地编辑游标槽位（草稿缓冲即槽位内容，
    /// 光标置于末尾；附件保留在槽位上，不随文本进缓冲）。
    fn queue_begin_edit(&mut self) {
        let Some(text) = self.queue.current_slot_text() else {
            return;
        };
        self.input.set_text(text);
        self.queue.begin_edit();
    }

    /// QUEUE `o`/`O`：在游标下/上方插入空槽位并就地编辑（保存空文本
    /// 即撤销该槽位，与保存语义一致）。
    fn queue_insert_slot(&mut self, offset: usize) {
        self.queue.insert_slot(offset);
        self.queue_begin_edit();
    }

    /// 保存就地编辑：写回槽位；空文本删除槽位（oil.nvim 空行忽略
    /// 语义）。队列清空时退出 QUEUE 回 NORMAL。
    fn queue_save_edit(&mut self) {
        let Some(slot) = self.queue.take_editing() else {
            return;
        };
        let text = self.input.text().trim().to_string();
        self.input.clear_buffer();
        if self.queue.save_edit(slot, text) {
            self.mode = Mode::Normal;
            self.notice = Some("队列已清空".to_string());
        }
    }

    /// NORMAL `m`：进入 QUEUE 模式（oil.nvim 式队列编辑）。队列为空
    /// 或草稿非空时拒绝并提示；进入即冻结队列注入——用户手持缓冲
    /// 编辑时 run 仍在推进，不冻结会让 core 在 turn 边界弹走条目
    /// 导致游标下标漂移。
    fn enter_queue(&mut self) {
        if self.queue.is_empty() {
            self.notice = Some("队列为空：运行中 Enter 排队".to_string());
            return;
        }
        if !self.input.text().is_empty() {
            self.notice = Some("草稿非空：i 继续编辑，或清空后再进队列".to_string());
            return;
        }
        self.mode = Mode::Queue;
        self.queue.freeze();
        self.pending_key = None;
        self.queue.reset();
    }

    /// QUEUE 导航子状态的 Esc：退出回 NORMAL，解冻队列注入；
    /// QUEUE 打开期间冻结的发送在退出时恢复——空闲且队列非空即取出
    /// 队首提交，运行中则由本轮结束后的自动 drain 继续。
    fn leave_queue(&mut self) -> Vec<Effect> {
        self.mode = Mode::Normal;
        self.queue.unfreeze();
        self.queue.end_edit();
        self.pending_key = None;
        if self.running {
            return Vec::new();
        }
        self.drain_queue().into_iter().collect()
    }

    /// QUEUE 模式是否打开（drain 冻结与渲染布局用）。
    pub(super) fn queue_mode_active(&self) -> bool {
        self.mode == Mode::Queue
    }

    /// 输入框队列区展示行数：各条目逻辑行数之和
    ///（就地编辑的槽位按草稿缓冲行数计）。
    pub(super) fn queue_display_lines(&self) -> u16 {
        let mut total = 0_u16;
        for (index, entry) in self.queue.entries().iter().enumerate() {
            let lines = if self.queue.editing_slot() == Some(index) {
                self.input.line_count()
            } else {
                line_count_of(&entry.text)
            };
            total = total.saturating_add(lines);
        }
        total
    }

    /// slash 命令的内部处置：能就地完成的直接做，需要外部资源的转为效果。
    fn execute_slash(&mut self, action: SlashAction) -> Vec<Effect> {
        match action {
            SlashAction::Help => {
                self.chat.push_system(help_text());
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
                self.chat.pop_trailing_failed_assistant();
                self.running = true;
                self.notice = None;
                vec![Effect::Retry]
            }
            SlashAction::Resume => vec![Effect::ListSessions],
            SlashAction::Models(None) => vec![Effect::ListModels],
            SlashAction::Models(Some(id)) => vec![Effect::SwitchModel(id)],
            SlashAction::Skill(None) => vec![Effect::ListSkills],
            SlashAction::Skill(Some(invocation)) => vec![Effect::LoadSkill(invocation)],
            SlashAction::Image(path) => vec![Effect::AttachImage(path)],
            SlashAction::Copy => self.copy_latest(),
            SlashAction::Thinking => {
                self.thinking_collapsed = !self.thinking_collapsed;
                let state = if self.thinking_collapsed {
                    "已折叠"
                } else {
                    "已展开"
                };
                self.chat
                    .push_system(format!("thinking 显示：{state}（/thinking 切换）"));
                Vec::new()
            }
            SlashAction::Goal => {
                self.goal_mode = !self.goal_mode;
                let state = if self.goal_mode {
                    "已开启：react loop 停止时若 todo 未全部完成，将自动以 user 消息追问"
                } else {
                    "已关闭"
                };
                self.chat
                    .push_system(format!("goal 模式{state}（/goal 切换）"));
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

    /// `/skill`：刷新命令行补全快照并列出可用 skill（本地展示，不进上下文）。
    pub(super) fn show_skills(&mut self, skills: Vec<SkillEntry>) {
        self.chat.push_system(skill_list_text(&skills));
        self.command.set_available_skills(skills);
    }

    /// `/new`：清空聊天区开启新对话；session 切换由调用方随后经
    /// [`Self::set_session`] / [`Self::warn`] 回报。
    /// 排队消息属于旧对话的后续意图，随上下文一起清空。
    pub(super) fn start_new_conversation(&mut self) {
        self.chat.clear_items();
        self.queue.clear();
        self.context_tokens = 0;
        self.chat.push_system("已开启新对话，上下文已清空。");
    }

    /// 切换当前 session 标识（`/new` 新建或 `/resume` 恢复后）。
    pub(super) fn set_session(&mut self, session_id: String) {
        self.session_id = Some(session_id);
    }

    /// `/resume`：以恢复的历史消息替换聊天区并切换 session。
    /// 排队消息属于切换前对话的后续意图，随上下文一起清空。
    /// picker 确认后底层模式是 NORMAL（命令受理即回 NORMAL），游标需
    /// 立即定位到最新一条消息，否则 `v`/`yy` 报「没有可选择的消息」。
    pub(super) fn restore_conversation(&mut self, messages: &[Message], session_id: String) {
        self.chat.clear_items();
        self.queue.clear();
        self.load_history(messages);
        self.chat.move_cursor_to_last_message();
        self.session_id = Some(session_id);
    }

    /// `/tree` 选择器确认：以分支重放的消息替换聊天区（session 不变；
    /// 落库父指针切换由调用方随后完成）。
    /// 排队消息属于切换前分支的后续意图，随上下文一起清空。
    pub(super) fn restore_branch(&mut self, messages: &[Message]) {
        self.chat.clear_items();
        self.queue.clear();
        self.load_history(messages);
        self.chat.move_cursor_to_last_message();
    }

    // ── 粘贴与外部编辑器 ────────────────────────────────────────────────────

    /// 粘贴一段文本（可含换行；`\r\n` 统一为 `\n`），随后重算补全。
    pub(super) fn paste_text(&mut self, text: &str) {
        // 粘贴的意图是编辑：命令行粘贴进命令缓冲（留在 COMMAND）；
        // QUEUE 导航下先进入就地编辑（粘贴即修改游标槽位）；
        // 其余（NORMAL/SEARCH 等）先回 INSERT 编辑草稿（草稿保留）
        match self.mode {
            Mode::Command => {
                self.command.paste(text);
                return;
            }
            Mode::Queue if !self.queue.is_editing() => self.queue_begin_edit(),
            Mode::Queue => {}
            _ => self.mode = Mode::Insert,
        }
        self.input.paste(text);
    }

    /// 编辑器写回（INSERT `Ctrl+G` 外部编辑器退出，见 [`Effect::OpenEditor`]）：
    /// 编辑器内容整体替换输入缓冲（编辑器是权威副本）；空白内容保留
    /// 原草稿（保存空文件是常见误操作，不应清掉已有输入）。
    pub(super) fn apply_editor_result(&mut self, text: &str) {
        if !self.input.apply_editor_result(text) {
            self.notice = Some("编辑器内容为空，输入保留未变".to_string());
        }
    }

    // ── 选择器（/resume、/models、/tree 共用） ──────────────────────────────

    /// 打开 `/resume` 选择器（从头选中）；调用方保证候选非空。
    pub(super) fn open_resume_picker(&mut self, rows: Vec<PickerRow>) {
        self.picker = Some(Picker::resume(rows));
    }

    /// 打开 `/models` 选择器，预选中当前模型；调用方保证候选非空。
    pub(super) fn open_model_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        self.picker = Some(Picker::models(rows, selected));
    }

    /// 打开思考级别选择器（模型切换流程第二步，预选中当前级别）；
    /// 调用方保证候选非空。
    pub(super) fn open_reasoning_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        self.picker = Some(Picker::reasoning(rows, selected));
    }

    /// 打开 `/tree` 选择器（预选中 `selected`，通常是当前分支末端）；
    /// 调用方保证候选非空且 `selected` 落在可选行上。
    pub(super) fn open_tree_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        self.picker = Some(Picker::tree(rows, selected));
    }

    /// 当前选择器（渲染与键位路由用）。
    pub(super) const fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    /// Enter 确认：取出选中行的（种类, id）并关闭选择器。
    /// 过滤后无可见行或选中不可选行（`/tree` 的工具调用条目）时不确认、
    /// 保持打开。
    fn take_picker_selection(&mut self) -> Option<(PickerKind, String)> {
        let entry = self.picker.as_ref()?.selected_entry()?;
        self.picker = None;
        Some(entry)
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

    // ── 键位帮助弹层 ────────────────────────────────────────────────────────

    /// 键位帮助弹层是否打开（渲染用）。
    pub(super) const fn help_open(&self) -> bool {
        self.help_scroll.is_some()
    }

    /// 渲染同步帮助弹层滚动边界：钳制并返回生效偏移（同聊天区的
    /// clamp 口径；未打开时返回 0）。
    pub(super) fn clamp_help_scroll(&mut self, max_scroll: u16) -> u16 {
        let Some(scroll) = self.help_scroll else {
            return 0;
        };
        let effective = scroll.min(max_scroll);
        self.help_scroll = Some(effective);
        effective
    }

    // ── 复制菜单 ────────────────────────────────────────────────────────────

    /// 当前复制菜单（渲染用）。
    pub(super) const fn copy_menu(&self) -> Option<&CopyMenu> {
        self.copy_menu.as_ref()
    }

    // ── 渲染读接口 ──────────────────────────────────────────────────────────

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

/// 逐行步进：越过边界返回 `None`（钳制语义由调用方决定）。
/// 聊天区消息游标、队列游标与 picker 选中共用。
fn step_row(index: usize, direction: isize, len: usize) -> Option<usize> {
    let next = index.checked_add_signed(direction)?;
    (next < len).then_some(next)
}

/// 文本的逻辑行数（空文本为 1）：草稿与队列条目共用的行数口径。
fn line_count_of(text: &str) -> u16 {
    let count = text.bytes().filter(|b| *b == b'\n').count() + 1;
    u16::try_from(count).unwrap_or(u16::MAX)
}
#[cfg(test)]
// 测试数据包含模板占位符字面量（${2:-nomic} 等），并非格式化参数
#[allow(clippy::literal_string_with_formatting_args)]
mod tests {
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

    /// 打开命令输入框并键入命令文本（ADR-0020）：INSERT 下先 Esc 进
    /// NORMAL，`:` 打开命令行（预填 `/`），粘贴不含 `/` 前缀的命令文本。
    fn open_command(app: &mut App, text: &str) {
        if app.mode() == Mode::Insert {
            app.press(Key::Esc);
        }
        app.press(Key::Char(':'));
        app.paste_text(text);
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
        app.handle_event(&AgentEvent::MessageEnd(assistant_message(
            Vec::new(),
            StopReason::Error,
            Some("rate limited".to_string()),
        )));
        let Some(ChatItem::Assistant(item)) = app.chat.items.first() else {
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
        app.command.insert_char('/');
        assert!(app.command.completion().is_some());
        // 换行是空白字符，slash 补全随之关闭
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
    fn slash_completion_filters_by_prefix_and_tab_cycles() {
        let mut app = app();
        app.command.insert_char('/');
        let completion = app.command.completion().expect("/ 即弹出全部候选");
        assert_eq!(completion.candidates.len(), SLASH_COMMANDS.len());

        app.command.insert_char('n');
        let completion = app.command.completion().expect("/n 匹配 new");
        assert_eq!(candidate_fragments(completion), vec!["new"]);

        // Tab 接受候选
        app.command.tab_complete();
        assert_eq!(app.command.text(), "/new");
        // 精确匹配后仍显示（展示描述），且选中该项
        let completion = app.command.completion().expect("精确匹配仍显示候选");
        assert_eq!(completion.candidates[completion.selected].fragment(), "new");

        // 输入空格（进入参数区）后弹层消失
        app.command.insert_char(' ');
        assert!(app.command.completion().is_none());
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
            app.command.insert_char(c);
        }
        let completion = app.command.completion().expect("/ex 匹配别名 exit");
        assert_eq!(
            completion.candidates[completion.selected].fragment(),
            "quit"
        );

        // 未精确匹配时 Enter 先填入候选，不提交
        assert!(app.command.accept_completion_on_enter());
        assert_eq!(app.command.text(), "/quit");
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
        assert_eq!(app.picker().expect("picker").selected, 2);
        app.picker.as_mut().expect("picker").select(-5);
        assert_eq!(app.picker().expect("picker").selected, 0);

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
            collapsed: false,
        }));
        // thinking 不复制，多个正文块以空行连接
        let [Effect::CopyText(text)] = &app.execute_slash(SlashAction::Copy)[..] else {
            panic!("expected CopyText effect");
        };
        assert_eq!(text, "第一段正文\n\n第二段正文");

        // 最新一条是只有工具调用的 assistant 消息：向前找有正文的消息
        app.chat
            .items
            .push(ChatItem::Assistant(AssistantItem::default()));
        app.chat.items.push(ChatItem::User("最新问题".to_string()));
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
            .chat
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
            .chat
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
        assert_eq!(app.chat.items.len(), 1);
        assert!(matches!(app.chat.items[0], ChatItem::User(_)));
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
        assert_eq!(app.chat.items.len(), 1);
        assert!(matches!(app.chat.items[0], ChatItem::User(_)));
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
        assert_eq!(app.chat.items.len(), 2);
    }

    #[test]
    fn parse_slash_skill_uses_colon_argument() {
        let skill = |name: &str, args: Option<&str>| {
            SlashParse::Known(SlashAction::Skill(Some(SkillInvocation {
                name: name.to_string(),
                args: args.map(str::to_string),
            })))
        };
        assert_eq!(
            parse_slash("/skill"),
            SlashParse::Known(SlashAction::Skill(None))
        );
        assert_eq!(parse_slash("/skill:jujutsu"), skill("jujutsu", None));
        // 空参数等价于无参（列出清单）
        assert_eq!(
            parse_slash("/skill:"),
            SlashParse::Known(SlashAction::Skill(None))
        );
        // 名称后首个空白起为附带 args（可为含空格的自由文本）
        assert_eq!(
            parse_slash("/skill:review 只看 unsafe 块"),
            skill("review", Some("只看 unsafe 块"))
        );
        // `/skill name` 空白形式仍属于非法用法（避免与 prompt template 调用混淆）
        assert!(matches!(
            parse_slash("/skill jujutsu"),
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
        unique.command.set_available_templates(vec![template(
            "review",
            "Review $@",
            Some("<path>"),
        )]);
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

    /// 构造空闲态且队列中有两条排队消息的 App（第一条带一张图片附件）。
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

    /// QUEUE 导航：j/k 钳制移动、G/gg 跳队尾/队首、dd 删除游标条目，
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
        assert_eq!(app.chat.scroll(), u16::MAX, "g 滚到顶（渲染时钳到上限）");

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

    /// NORMAL `y` 复制菜单：预选中消息游标条目，Enter 复制；
    /// 重新打开后选中用户消息条目同样可复制。
    #[test]
    fn normal_y_copy_menu_copies_selected() {
        let mut app = app_with_history();
        app.press(Key::Esc);
        // 游标在最新 assistant（下标 4）：y 打开菜单预选其所在行，Enter 复制
        app.press(Key::Char('y'));
        assert_eq!(app.mode(), Mode::CopyMenu);
        let effects = app.press(Key::Enter);
        assert!(
            matches!(&effects[..], [Effect::CopyText(text)] if text.contains("看这里")),
            "{effects:?}"
        );

        // 游标移到 user 条目后重开菜单：预选 user 行，Enter 复制 user 文本
        app.press(Key::Char('['));
        app.press(Key::Char('y'));
        let effects = app.press(Key::Enter);
        assert!(
            matches!(&effects[..], [Effect::CopyText(text)] if text == "第二个问题"),
            "{effects:?}"
        );
    }

    /// NORMAL `y` 复制菜单的代码块行：数字键直达复制；j 导航后 Enter 复制。
    #[test]
    fn normal_y_copy_menu_code_blocks() {
        let mut app = app_with_history();
        app.press(Key::Esc);
        // 菜单行序（新条目在前）：助手消息、代码块 1/2、代码块 2/2、…
        app.press(Key::Char('y'));
        assert_eq!(app.mode(), Mode::CopyMenu);
        // 数字键 3 直达第二个代码块（行下标 2 = 代码块 2/2）
        let effects = app.press(Key::Char('3'));
        assert!(
            matches!(&effects[..], [Effect::CopyText(text)] if text == "第二块\n"),
            "{effects:?}"
        );
        assert_eq!(app.mode(), Mode::Normal, "复制后关闭菜单");

        // 重开菜单：j 选中代码块 1/2，Enter 复制
        app.press(Key::Char('y'));
        app.press(Key::Char('j'));
        let effects = app.press(Key::Enter);
        assert!(
            matches!(&effects[..], [Effect::CopyText(text)] if text == "fn main() {}\n"),
            "{effects:?}"
        );
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
        assert_eq!(app.search.matches, vec![0, 3]);
        assert_eq!(
            app.chat.cursor_item,
            Some(0),
            "游标在尾部，增量回绕到首个命中"
        );

        // Enter 保留命中；n 循环跳转
        app.press(Key::Enter);
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.notice(), Some("2 处命中 · n/N 跳转"));
        app.press(Key::Char('n'));
        assert_eq!(app.chat.cursor_item, Some(3), "n 循环到下一处");
        app.press(Key::Char('n'));
        assert_eq!(app.chat.cursor_item, Some(0));
        // N 反向
        app.press(Key::Char('N'));
        assert_eq!(app.chat.cursor_item, Some(3));

        // 再次 / 保留上次查询可编辑；Esc 清空
        app.press(Key::Char('/'));
        assert_eq!(app.search.query(), "问题");
        app.press(Key::Backspace);
        assert_eq!(app.search.query(), "问");
        app.press(Key::Esc);
        assert_eq!(app.mode(), Mode::Normal);
        assert_eq!(app.search.query(), "");
        assert!(app.search.highlight().is_none(), "Esc 清空高亮");
        // 无命中时 n 给提示
        assert!(app.press(Key::Char('n')).is_empty());
        assert_eq!(app.notice(), Some("没有搜索命中（NORMAL 下 / 开始搜索）"));
    }

    /// NORMAL：A/I 分别到输入末尾/行首回 INSERT（草稿编辑统一回 INSERT）。
    #[test]
    fn normal_a_i_return_to_insert_at_edges() {
        let mut app = app();
        app.paste_text("hello world foo");
        app.press(Key::Home);
        app.press(Key::Esc);
        assert_eq!(app.mode(), Mode::Normal);

        app.press(Key::Char('A'));
        assert_eq!(app.mode(), Mode::Insert);
        assert_eq!(app.input.cursor_position().1, 15);

        app.press(Key::Esc);
        app.press(Key::Char('I'));
        assert_eq!(app.mode(), Mode::Insert);
        assert_eq!(app.input.cursor_position().1, 0);
    }

    /// NORMAL `Space`：切换游标条目折叠（assistant/tool），user/system 不可折叠提示。
    #[test]
    fn normal_space_toggles_item_collapse() {
        let mut app = app_with_history();
        app.press(Key::Esc);
        // 游标在最新 assistant（下标 4）
        assert!(app.chat.cursor_item.is_some());
        app.press(Key::Char(' '));
        assert!(matches!(
            &app.chat.items()[4],
            ChatItem::Assistant(a) if a.collapsed
        ));
        app.press(Key::Char(' '));
        assert!(matches!(
            &app.chat.items()[4],
            ChatItem::Assistant(a) if !a.collapsed
        ));

        // 游标移到工具条目（{ 向前到 tool），Space 折叠工具
        app.press(Key::Char('{'));
        app.press(Key::Char(' '));
        assert!(matches!(
            &app.chat.items()[2],
            ChatItem::Tool(t) if t.collapsed
        ));

        // 游标移到 user 条目（[ 连按两次：tool→assistant→user），Space 不可折叠
        app.press(Key::Char('['));
        app.press(Key::Char('['));
        assert_eq!(app.chat.cursor_item, Some(0));
        app.press(Key::Char(' '));
        assert!(app.notice().is_some_and(|n| n.contains("不可折叠")));
    }

    /// 消息游标滚动定位：渲染回写条目行号后，移动游标滚动到该条目。
    #[test]
    fn cursor_movement_scrolls_to_item() {
        let mut app = app_with_history();
        // 模拟渲染回写：条目 0..=4 起始行 0,10,20,30,40；scroll_max 50
        app.chat.sync_item_lines(vec![0, 10, 20, 30, 40]);
        app.chat.clamp_scroll(50);
        app.press(Key::Esc);
        assert_eq!(app.chat.cursor_item, Some(4));
        app.press(Key::Char('['));
        // 条目 3 起始行 30：scroll = 50 - 30
        assert_eq!(app.chat.scroll(), 20);
        app.press(Key::Char('g'));
        assert_eq!(app.chat.scroll(), u16::MAX, "g 仍然直接滚到顶");
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
        open_command(&mut app, "models");
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::ListModels]));

        open_command(&mut app, "models:gpt-5.2");
        let effects = app.press(Key::Enter);
        assert!(matches!(&effects[..], [Effect::SwitchModel(id)] if id == "gpt-5.2"));

        app.set_model("GPT-5.2".to_string(), 400_000);
        assert_eq!(app.model_name(), "GPT-5.2");
        assert_eq!(app.context_window(), 400_000);
    }

    #[test]
    fn unknown_and_invalid_slash_warn_via_notice() {
        let mut unknown = app();
        open_command(&mut unknown, "foobar");
        assert!(unknown.press(Key::Enter).is_empty());
        assert!(unknown.notice().is_some_and(|n| n.contains("未知命令")));
        assert_eq!(unknown.mode(), Mode::Command, "被拒绝时留在命令行");

        let mut invalid = app();
        open_command(&mut invalid, "skill a b");
        assert!(invalid.press(Key::Enter).is_empty());
        assert!(invalid.notice().is_some_and(|n| n.contains("用法")));
        assert_eq!(invalid.mode(), Mode::Command, "被拒绝时留在命令行");
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
        app.chat.push_system("旧内容");
        app.restore_conversation(&[*user_message("恢复的")], "sid-1".to_string());
        assert_eq!(app.chat.items().len(), 1);
        assert!(matches!(&app.chat.items()[0], ChatItem::User(t) if t == "恢复的"));
        assert_eq!(app.session_id(), Some("sid-1"));
    }

    /// picker 确认恢复后底层模式是 NORMAL（命令受理即回 NORMAL，picker 是
    /// 派生态），消息游标必须立即有效：恢复前无消息（游标 None）或游标
    /// 停在旧会话的越界下标时，`y` 复制菜单不应报「没有可复制的消息」。
    #[test]
    fn restore_conversation_positions_cursor_on_last_message() {
        // 恢复前无消息：游标 None
        let mut app = app();
        app.press(Key::Esc);
        assert_eq!(app.chat.cursor_item, None);
        app.restore_conversation(
            &[
                *user_message("问题"),
                *assistant_message(vec![text_block("回答")], StopReason::Stop, None),
            ],
            "sid-1".to_string(),
        );
        assert_eq!(app.chat.cursor_item, Some(1), "游标定位到最新一条消息");
        // y 复制菜单打开（游标有效）：预选最新消息，Enter 复制
        app.press(Key::Char('y'));
        assert_eq!(app.mode(), Mode::CopyMenu);
        let [Effect::CopyText(text)] = &app.press(Key::Enter)[..] else {
            panic!("恢复后 y 菜单应可复制游标消息");
        };
        assert_eq!(text, "回答");

        // 恢复前游标停在旧会话的大下标（越界）
        let mut app = app_with_history();
        app.press(Key::Esc);
        assert_eq!(app.chat.cursor_item, Some(4));
        app.restore_conversation(&[*user_message("短历史")], "sid-2".to_string());
        assert_eq!(app.chat.cursor_item, Some(0), "越界游标被重置到新历史内");
        app.press(Key::Char('y'));
        let [Effect::CopyText(text)] = &app.press(Key::Enter)[..] else {
            panic!("恢复后 y 菜单应可复制游标消息");
        };
        assert_eq!(text, "短历史");
    }

    /// `/tree` 分支重放与 `/resume` 同一口径：游标定位到重放后最新一条消息。
    #[test]
    fn restore_branch_positions_cursor_on_last_message() {
        let mut app = app();
        app.press(Key::Esc);
        app.restore_branch(&[
            *user_message("分支起点"),
            *assistant_message(vec![text_block("分支回答")], StopReason::Stop, None),
        ]);
        assert_eq!(app.chat.cursor_item, Some(1));
        app.press(Key::Char('y'));
        assert_eq!(app.mode(), Mode::CopyMenu);
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
        open_command(&mut app, "tree");
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
        app.chat.push_system("旧内容");
        app.restore_branch(&[*user_message("分支起点")]);
        assert_eq!(app.chat.items().len(), 1);
        assert!(matches!(&app.chat.items()[0], ChatItem::User(t) if t == "分支起点"));
        assert_eq!(app.session_id(), Some("sid-1"));
    }

    /// HELP 弹层（NORMAL `?`）：打开派生 Help 模式，Esc/q/`?` 关闭后
    /// 回到 NORMAL（底层 mode 字段未动）；j/k 滚动、gg/G 顶/底，
    /// 上限由渲染回写钳制。
    #[test]
    fn help_overlay_opens_scrolls_and_closes() {
        let mut app = app();
        // INSERT 下 `?` 是普通字符（输入语义不被抢占）
        app.press(Key::Char('?'));
        assert_eq!(app.input.text(), "?");
        assert_eq!(app.mode(), Mode::Insert);

        app.press(Key::Esc);
        assert_eq!(app.mode(), Mode::Normal);
        // 打开：派生 Help 模式
        assert!(app.press(Key::Char('?')).is_empty());
        assert_eq!(app.mode(), Mode::Help);
        assert!(app.help_open());

        // 滚动：k 在顶部不动，j 下移，渲染钳制上限
        app.press(Key::Char('k'));
        assert_eq!(app.help_scroll, Some(0));
        app.press(Key::Char('j'));
        app.press(Key::Char('j'));
        assert_eq!(app.help_scroll, Some(2));
        assert_eq!(app.clamp_help_scroll(1), 1, "渲染钳制到上限");
        assert_eq!(app.help_scroll, Some(1));
        // G 到底（经钳制生效）、gg 回顶
        app.press(Key::Char('G'));
        assert_eq!(app.clamp_help_scroll(5), 5);
        app.press(Key::Char('g'));
        app.press(Key::Char('g'));
        assert_eq!(app.help_scroll, Some(0));

        // 其余按键不污染输入缓冲（打开前的草稿 `?` 原样保留）
        assert!(app.press(Key::Char('x')).is_empty());
        assert_eq!(app.input.text(), "?");

        // Esc 关闭，回到 NORMAL
        assert!(app.press(Key::Esc).is_empty());
        assert_eq!(app.mode(), Mode::Normal);
        assert!(!app.help_open());

        // q / ? 同样关闭
        app.press(Key::Char('?'));
        app.press(Key::Char('q'));
        assert_eq!(app.mode(), Mode::Normal);
        app.press(Key::Char('?'));
        app.press(Key::Char('?'));
        assert_eq!(app.mode(), Mode::Normal);
    }

    /// NORMAL `g` 是完整动作（less 式到顶，无 pending 状态），随后 `?`
    /// 正常打开帮助弹层。
    #[test]
    fn help_opens_after_scroll_to_top() {
        let mut app = app();
        app.press(Key::Esc);
        app.press(Key::Char('g'));
        assert_eq!(app.chat.scroll(), u16::MAX);
        assert!(app.press(Key::Char('?')).is_empty());
        assert_eq!(app.mode(), Mode::Help);
    }
}
