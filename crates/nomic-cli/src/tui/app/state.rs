//! App 的第一组方法：构造/访问器/事件处理/模式路由与 INSERT 按键（由 app/mod.rs 的 `impl App` 拆分而来）。

use std::time::Instant;

use super::{
    AgentEvent, App, Chat, Effect, HALF_PAGE_SCROLL, Input, Key, Message, Mode, PAGE_SCROLL,
    PromptsError, QUIT_CONFIRM_TIMEOUT, Queue, SlashParse, ToolItem, ToolStatus, assistant_error,
    brief_args, estimate_context_tokens, parse_slash, user_text,
};

impl App {
    pub fn new(model_name: String, session_id: Option<String>, context_window: u64) -> Self {
        let mut input = Input::new();
        // 草稿不承载命令（ADR-0020）：slash 补全只属于命令输入框；
        // `@` mention 补全属于草稿
        input.set_completion_enabled(false);
        let mut command = Input::new();
        // 命令输入框只承载 slash 命令，不启用 `@` mention 补全
        command.set_mention_enabled(false);
        Self {
            chat: Chat::default(),
            input,
            command,
            queue: Queue::default(),
            picker: None,
            mode: Mode::Insert,
            pending_key: None,
            history: Vec::new(),
            history_index: None,
            history_stash: String::new(),
            help_scroll: None,
            running: false,
            should_quit: false,
            quit_armed: None,
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

    /// 聊天区状态（条目、滚动与几何）。
    pub const fn chat(&self) -> &Chat {
        &self.chat
    }

    /// 聊天区状态（可变）：滚动、系统提示与条目操作用。
    pub const fn chat_mut(&mut self) -> &mut Chat {
        &mut self.chat
    }

    /// 渲染前按已知视口刷新聊天区几何（条目起始行、滚动上限并就地钳制
    /// 滚动）：「宽度 → 折行 → 条目起始行」在状态层主动计算，渲染 widget
    /// 只读。每帧 draw 渲染聊天区前调用；测试可直接调用，无需先渲一帧。
    pub fn sync_chat_geometry(&mut self, width: u16, height: u16) {
        let thinking_collapsed = self.thinking_collapsed;
        let spinner = self.spinner();
        self.chat
            .sync_geometry(width, height, thinking_collapsed, spinner);
    }

    /// 输入区状态（草稿、补全、附件）。
    pub const fn input(&self) -> &Input {
        &self.input
    }

    /// 输入区状态（可变）：附件暂存用。
    pub const fn input_mut(&mut self) -> &mut Input {
        &mut self.input
    }

    /// 命令输入框状态（COMMAND 模式渲染用）。
    pub const fn command(&self) -> &Input {
        &self.command
    }

    /// 命令输入框状态（可变）：skill/template 补全快照用。
    pub const fn command_mut(&mut self) -> &mut Input {
        &mut self.command
    }

    /// 队列状态（条数、条目视图、QUEUE 模式游标）。
    pub const fn queue(&self) -> &Queue {
        &self.queue
    }

    // ── 事件与历史 ──────────────────────────────────────────────────────────

    /// 把 resume 恢复的历史消息渲染为聊天条目。
    pub fn load_history(&mut self, messages: &[Message]) {
        self.context_tokens = estimate_context_tokens(messages);
        self.chat.load_history(messages);
    }

    /// 消费一个 agent 事件，更新状态。
    pub fn handle_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentStart => self.running = true,
            AgentEvent::MessageStart(message) => match message.as_ref() {
                Message::User(user) => {
                    self.chat.push_user_text(&user_text(&user.content));
                }
                Message::Assistant(_) => self.chat.start_assistant(),
                Message::ToolResult(_) => {}
            },
            AgentEvent::MessageUpdate(delta) => self.chat.apply_delta(delta),
            AgentEvent::MessageEnd {
                message,
                context_tokens,
            } => {
                // 权威上下文估算随事件携带（锚点规则只在 core 定义一次），
                // 这里只抄不算
                self.context_tokens = *context_tokens;
                if let Message::Assistant(assistant) = message.as_ref() {
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
                context_tokens,
                kept_count,
                ..
            } => {
                self.notice = None;
                self.context_tokens = *context_tokens;
                self.chat.push_system(format!(
                    "上下文已压缩：约 {tokens_before} tokens → 摘要 + {kept_count} 条近期消息。"
                ));
            }
            AgentEvent::AgentEnd { context_tokens, .. } => {
                // job 完成事件携带的权威值（与末尾 MessageEnd 同口径），只抄
                self.context_tokens = *context_tokens;
            }
            AgentEvent::TurnStart | AgentEvent::TurnEnd { .. } => {}
        }
    }

    // ── 按键（语义分发） ────────────────────────────────────────────────────

    /// 当前交互模式（渲染光标/徽标与外部查询用）：picker/帮助弹层
    /// 打开时派生为对应模式，否则为字段值（Insert/Normal）。
    pub const fn mode(&self) -> Mode {
        if self.picker.is_some() {
            Mode::Picker
        } else if self.help_scroll.is_some() {
            Mode::Help
        } else {
            self.mode
        }
    }

    /// 消费一个按键，返回需要事件循环接线执行的语义效果。
    /// 按交互模式分发（ADR-0011）：picker/补全/命令的路由全部
    /// 在此内部完成。
    pub fn press(&mut self, key: Key) -> Vec<Effect> {
        // 退出确认态的解除（ADR-0021 修订）：超时解除；确认键（NORMAL
        // 下的 `q`）以外的任意按键解除（「按其他键继续」）
        if self.quit_armed.is_some() {
            let confirm = self.mode() == Mode::Normal && key == Key::Char('q');
            if !confirm || !self.quit_armed_pending() {
                self.disarm_quit();
            }
        }
        match self.mode() {
            // 选择器打开时接管键位（命令仅在空闲时可提交，
            // 此时 agent 必空闲，无运行可取消）
            Mode::Picker => self.press_picker(key),
            Mode::Help => self.press_help(key),
            Mode::Normal => self.press_normal(key),
            Mode::Insert => self.press_insert(key),
            Mode::Command => self.press_command(key),
            Mode::Queue => self.press_queue(key),
        }
    }

    /// INSERT 模式键位（ADR-0021）：编辑与提交 prompt；`Esc` 回 NORMAL
    ///（运行中亦然；中断/退出在 NORMAL 按 `q`）、`Ctrl+C` 清草稿/退出、
    /// `Ctrl+D` 空草稿退出/非空删字符、`↑/↓` 历史召回。命令不在此触发
    ///（ADR-0020）：`/` 开头的输入按普通 prompt 发送，命令走 COMMAND 模式。
    pub fn press_insert(&mut self, key: Key) -> Vec<Effect> {
        match key {
            // Ctrl+C：清草稿（含附件）；草稿已空时退出（运行中先中断再退出）
            Key::Ctrl('c') => {
                if !self.input.text().is_empty() || self.input.has_attachments() {
                    self.input.clear_draft();
                    self.history_index = None;
                    self.notice = None;
                } else {
                    return self.quit();
                }
            }
            // Ctrl+D：空草稿退出（EOF 惯例）；非空删除光标处字符（readline）
            Key::Ctrl('d') => {
                if self.input.text().is_empty() && !self.input.has_attachments() {
                    return self.quit();
                }
                self.input.delete_char_at_cursor();
            }
            // Esc：先关 `@` mention 补全弹层，否则回 NORMAL（逐层退回，ADR-0021）
            Key::Esc => {
                if self.input.dismiss_mention() {
                    return Vec::new();
                }
                self.enter_normal();
            }
            // Tab：mention 补全弹层可见时接受选中候选（填入标记，不发送）
            Key::Tab => {
                self.input.mention_tab_complete();
                return Vec::new();
            }
            // ↑/↓：mention 补全弹层可见时移动选中，否则历史召回
            Key::Up => {
                if self.input.mention().is_some() {
                    self.input.mention_select(-1);
                } else {
                    self.history_prev();
                }
            }
            Key::Down => {
                if self.input.mention().is_some() {
                    self.input.mention_select(1);
                } else {
                    self.history_next();
                }
            }
            // Ctrl+G：外部编辑器编辑当前草稿（长文/多行场景；编辑器持有
            // 草稿副本，保存退出后整体写回，放弃则原样保留）
            Key::Ctrl('g') => return vec![Effect::OpenEditor],
            Key::Enter => return self.press_enter(),
            other => Self::edit_key(&mut self.input, &mut self.chat, other),
        }
        Vec::new()
    }

    /// ↑：召回上一条历史（新条目在前，索引 0 最新；首次召回前暂存当前
    /// 草稿供 ↓ 还原）。
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.history_stash = self.input.text().to_string();
                self.history_index = Some(0);
            }
            Some(index) if index + 1 < self.history.len() => {
                self.history_index = Some(index + 1);
            }
            Some(_) => return,
        }
        let index = self.history_index.expect("index just set");
        self.input.set_text(self.history[index].clone());
    }

    /// ↓：向新方向召回；到最新时还原暂存草稿并退出召回。
    pub fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(0) => {
                self.input.set_text(std::mem::take(&mut self.history_stash));
                self.history_index = None;
            }
            Some(index) => {
                self.history_index = Some(index - 1);
                self.input.set_text(self.history[index - 1].clone());
            }
        }
    }

    /// 记录一条已提交的 prompt 到历史（去重相邻重复，新条目在前）。
    pub fn record_history(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.history.first().is_some_and(|last| last == text) {
            return;
        }
        self.history.insert(0, text.to_string());
        // 历史上限：只保留最近 200 条，避免长期会话无限增长
        self.history.truncate(200);
        self.history_index = None;
    }

    /// 缓冲编辑键（INSERT、COMMAND 与 QUEUE 就地编辑共用）：字符输入、
    /// 删除、光标移动、换行与聊天区滚动；提交、补全与模式切换由调用方
    /// 各自处理。
    pub fn edit_key(input: &mut Input, chat: &mut Chat, key: Key) {
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
    pub const fn edit_vertical(input: &mut Input, chat: &mut Chat, delta: isize) {
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
    pub fn press_command(&mut self, key: Key) -> Vec<Effect> {
        match key {
            // Ctrl+C/D：退出（取消运行归 NORMAL `q`/`Esc`）
            Key::Ctrl('c' | 'd') => return self.quit(),
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
    pub fn enter_command(&mut self) {
        self.mode = Mode::Command;
        self.pending_key = None;
        self.command.set_text(String::new());
        self.command.insert_char('/');
    }

    /// 离开 COMMAND 回 NORMAL：清空命令缓冲（无论已执行还是放弃）。
    pub fn leave_command(&mut self) {
        self.mode = Mode::Normal;
        self.pending_key = None;
        self.command.set_text(String::new());
    }

    /// COMMAND 的 Enter：空命令行（仅预填的 `/`）无声返回 NORMAL；
    /// 补全弹层未精确匹配时先填入候选；其余按命令分发——被拒绝（参数
    /// 非法、未知命令、运行中会话命令）时留在 COMMAND 供修正，受理后
    /// 回 NORMAL（vim `:` 执行完回 normal 的同一口径）。
    pub fn command_enter(&mut self) -> Vec<Effect> {
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
    pub fn dispatch_command(&mut self, text: &str) -> Option<Vec<Effect>> {
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
    /// d/u 半页、g/G 顶底）、`Y` 复制最新消息、`m` 队列、`r` 重试、
    /// `e` 编辑器、`q` 中断运行/退出；输入字符不进入缓冲（草稿保留，
    /// `i`/`a`/`Enter` 回到 INSERT 继续编辑）。NORMAL 是纯浏览态，不持有
    /// 消息游标（ADR-0026）。
    pub fn press_normal(&mut self, key: Key) -> Vec<Effect> {
        if let Some(effects) = self.normal_exit(key) {
            return effects;
        }
        match key {
            // g/G：到顶/回底（less 惯例；渲染前经 sync_chat_geometry 钳到上限）
            Key::Char('g') => self.chat.scroll_up(u16::MAX),
            Key::Char('G') => self.chat.scroll_to_bottom(),
            // d/u：半页下/上（less 惯例）
            Key::Char('d') => self.chat.scroll_down(HALF_PAGE_SCROLL),
            Key::Char('u') => self.chat.scroll_up(HALF_PAGE_SCROLL),
            // Y：直接复制最新一条消息（等价 /copy）
            Key::Char('Y') => return self.copy_latest(),
            // m：队列编辑 overlay（oil.nvim 式，ADR-0014）
            Key::Char('m') => self.enter_queue(),
            // s/b/c：会话命令直达（恢复 / 分支树 / 新建，ADR-0021 修订）
            Key::Char('s') => return self.session_command(Effect::ListSessions),
            Key::Char('b') => return self.session_command(Effect::ListTree),
            Key::Char('c') => return self.session_command(Effect::NewSession),
            // r：重试最近失败的一轮（与 /retry 同一口径；运行中拒绝）
            Key::Char('r') => return self.retry_last(),
            // e：外部编辑器编辑草稿（与 INSERT Ctrl+G 同一效果）
            Key::Char('e') => return vec![Effect::OpenEditor],
            // `?` 打开键位帮助弹层（只读；Esc/q/`?` 关闭）
            Key::Char('?') => return self.open_help(),
            // q：退出当前活动（ADR-0021 修订）：有未交付意图（运行中 /
            // 未发送草稿 / 排队消息）时第一次按进入确认态（运行中先中断
            // 本轮），确认态中第二次按退出；干净空闲直接退出
            Key::Char('q') => return self.request_quit(),
            // Ctrl+C：退出（运行中先中断再退出）
            Key::Ctrl('c') => return self.quit(),
            Key::Char('k') | Key::Up => self.chat.scroll_up(1),
            Key::Char('j') | Key::Down => self.chat.scroll_down(1),
            Key::PageUp => self.chat.scroll_up(PAGE_SCROLL),
            Key::PageDown => self.chat.scroll_down(PAGE_SCROLL),
            // 其余按键（含普通字符）忽略：不污染输入缓冲
            _ => {}
        }
        Vec::new()
    }

    /// NORMAL `q`：退出当前活动（ADR-0021 修订）。有未交付意图（运行中 /
    /// 未发送草稿或附件 / 排队消息）时第一次按进入确认态——运行中先中断
    /// 本轮，notice 提示再按确认；确认态中第二次按退出。干净空闲态直接
    /// 退出（session 已落库，退出零损失）。
    pub fn request_quit(&mut self) -> Vec<Effect> {
        if self.quit_armed_pending() {
            return self.quit();
        }
        if self.running {
            self.arm_quit("已中断本轮；再按 q 退出，按其他键继续".to_string());
            return vec![Effect::Cancel];
        }
        if !self.input.text().is_empty() || self.input.has_attachments() {
            self.arm_quit("草稿未发送；再按 q 退出并丢弃，按其他键继续".to_string());
            return Vec::new();
        }
        if !self.queue.is_empty() {
            self.arm_quit(format!(
                "{} 条排队消息；再按 q 退出并丢弃，按其他键继续",
                self.queue.len()
            ));
            return Vec::new();
        }
        self.quit()
    }

    /// 进入退出确认态：记录时间点并置提示。
    fn arm_quit(&mut self, notice: String) {
        self.quit_armed = Some(Instant::now());
        self.notice = Some(notice);
    }

    /// 解除退出确认态（超时 / 按其他键 / 运行结束）：连带清掉确认提示。
    fn disarm_quit(&mut self) {
        self.quit_armed = None;
        self.notice = None;
    }

    /// 退出确认态是否仍在有效期内（超时视为已解除）。
    pub(super) fn quit_armed_pending(&self) -> bool {
        self.quit_armed
            .is_some_and(|armed_at| armed_at.elapsed() <= QUIT_CONFIRM_TIMEOUT)
    }

    /// 退出 TUI（NORMAL 确认态的第二次 `q`、各模式 `Ctrl+C`）：运行中先
    /// 中断本轮再退出。
    pub fn quit(&mut self) -> Vec<Effect> {
        self.quit_armed = None;
        self.should_quit = true;
        if self.running {
            return vec![Effect::Cancel];
        }
        Vec::new()
    }

    /// NORMAL `r`：重试最近失败的一轮（与 `/retry` 同一口径）；
    /// 运行中拒绝并提示。
    pub fn retry_last(&mut self) -> Vec<Effect> {
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
    pub fn normal_exit(&mut self, key: Key) -> Option<Vec<Effect>> {
        match key {
            // Esc 退出当前界面层（ADR-0021 修订）：纯结构导航回 INSERT，
            // 永不中断运行（中断归 `q` 确认态的第一阶段）；
            // i/a 回到光标原处继续编辑——效果与 Esc 相同，合并处理
            Key::Esc | Key::Char('i' | 'a') => self.leave_normal(),
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

    /// 进入 NORMAL：草稿保留。
    pub const fn enter_normal(&mut self) {
        self.mode = Mode::Normal;
        self.pending_key = None;
    }

    /// 离开 NORMAL 回 INSERT：清掉序列键 pending，避免残留的首键
    /// 在下次进入 NORMAL 时被误当第二键。
    pub const fn leave_normal(&mut self) {
        self.mode = Mode::Insert;
        self.pending_key = None;
    }
}
