//! App 的第二组方法：复制/选择器/队列/help/会话与 slash 执行（由 app/mod.rs 的 `impl App` 拆分而来）。

use super::{
    App, CopyMenu, Effect, HALF_PAGE_SCROLL, Key, Message, Mode, PAGE_SCROLL, PICKER_PAGE_SCROLL,
    Picker, PickerKind, PickerRow, SPINNER_FRAMES, SkillEntry, SlashAction, SteeringMessage,
    help_text, line_count_of, skill_list_text,
};

impl App {
    /// 复制最新一条消息到剪贴板（`/copy` 与 NORMAL `Y` 共用）。
    pub fn copy_latest(&mut self) -> Vec<Effect> {
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
    pub fn press_picker(&mut self, key: Key) -> Vec<Effect> {
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
                    return match kind {
                        PickerKind::Resume => vec![Effect::Resume(id)],
                        PickerKind::Tree => vec![Effect::BranchTo(id)],
                        PickerKind::Models => vec![Effect::SwitchModel(id)],
                        PickerKind::Reasoning => vec![Effect::SetReasoning(id)],
                    };
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
    pub fn picker_select(&mut self, delta: isize) {
        if let Some(picker) = &mut self.picker {
            picker.select(delta);
        }
    }

    /// 跳转 picker 选中到可见行的 `pos`（picker 打开时）。
    pub fn picker_jump(&mut self, pos: usize, direction: isize) {
        if let Some(picker) = &mut self.picker {
            picker.jump(pos, direction);
        }
    }

    /// INSERT 的 Enter：取出草稿提交 prompt（运行中的口径见
    /// [`Self::press_enter_running`]）。命令不在此触发（ADR-0020）：
    /// `/` 开头的草稿同样按普通 prompt 发送，命令走 COMMAND 模式
    ///（NORMAL `:` 打开命令输入框）。
    pub fn press_enter(&mut self) -> Vec<Effect> {
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
        self.record_history(&text);
        // AgentStart 事件也会置位；先置避免提交空窗期重复提交
        self.running = true;
        self.notice = None;
        vec![Effect::Prompt { text, images }]
    }

    /// 运行中（含工具执行中）的 INSERT Enter：普通输入**排队**——入
    /// 统一消息队列（ADR-0014，当前 turn 的工具调用执行完后注入本轮
    /// 运行）；Esc→NORMAL→m 进 QUEUE 模式编辑队列。运行中执行命令走
    /// COMMAND 模式（本地命令照常，会话命令仍须等本轮结束）。
    pub fn press_enter_running(&mut self) -> Vec<Effect> {
        let Some(text) = self.input.take_input() else {
            if self.input.has_attachments() {
                self.notice = Some("已附加图片，输入文本后 Enter 一起排队".to_string());
            }
            return Vec::new();
        };
        self.record_history(&text);
        self.enqueue(text)
    }

    // ── 排队输入与 QUEUE 模式（ADR-0014）───────────────────────────

    /// 入队（ADR-0014，统一消息队列）：随暂存附件一起入队，当前 turn
    /// 的工具调用执行完后由 core 在 turn 边界注入本轮运行（run 异常
    /// 结束时保留，恢复后作为下一轮 prompt）；Esc→NORMAL→m 进 QUEUE
    /// 模式可编辑（编辑期间冻结注入）。
    pub fn enqueue(&mut self, text: String) -> Vec<Effect> {
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
    pub fn drain_queue(&mut self) -> Option<Effect> {
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
    pub fn press_queue(&mut self, key: Key) -> Vec<Effect> {
        if self.queue.is_editing() {
            return self.press_queue_edit(key);
        }
        // 序列键第二键（dd 删除）；不匹配照常分发
        if let Some(pending) = self.pending_key.take()
            && let Some(effects) = self.queue_sequence(pending, key)
        {
            return effects;
        }
        match key {
            Key::Char('d') => self.pending_key = Some('d'),
            Key::Char('g') => self.queue.jump_to_first(),
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

    /// QUEUE 的序列键第二键：`dd` 删除游标条目。
    /// 返回 `Some` 表示已处理。
    pub fn queue_sequence(&mut self, pending: char, key: Key) -> Option<Vec<Effect>> {
        match (pending, key) {
            ('d', Key::Char('d')) => self.queue_delete(),
            _ => return None,
        }
        Some(Vec::new())
    }

    /// QUEUE 编辑子状态键位：Enter/Esc 保存（vim 保存即应用），
    /// 其余按键与 INSERT 的缓冲编辑一致（补全在 QUEUE 下不启用）。
    pub fn press_queue_edit(&mut self, key: Key) -> Vec<Effect> {
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
    pub const fn open_help(&mut self) -> Vec<Effect> {
        self.help_scroll = Some(0);
        Vec::new()
    }

    /// HELP 弹层键位（NORMAL `?` 打开）：只读浏览，j/k 等滚动、
    /// g/G 到顶/底；Esc/q/`?` 关闭回到底层模式（mode 字段未动，
    /// 天然回到打开前的 NORMAL）。其余按键忽略，不污染输入缓冲。
    pub fn press_help(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Esc | Key::Char('q' | '?') => self.help_scroll = None,
            // g 到顶：渲染时由帮助 widget 钳到实际上限
            Key::Char('g') => self.help_scroll = Some(0),
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
    pub fn help_scroll_by(&mut self, delta: i32) {
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
    pub fn queue_delete(&mut self) {
        if self.queue.delete() {
            self.mode = Mode::Normal;
            self.notice = Some("队列已清空".to_string());
        }
    }

    /// QUEUE `i`/`a`/Enter：开始就地编辑游标槽位（草稿缓冲即槽位内容，
    /// 光标置于末尾；附件保留在槽位上，不随文本进缓冲）。
    pub fn queue_begin_edit(&mut self) {
        let Some(text) = self.queue.current_slot_text() else {
            return;
        };
        self.input.set_text(text);
        self.queue.begin_edit();
    }

    /// QUEUE `o`/`O`：在游标下/上方插入空槽位并就地编辑（保存空文本
    /// 即撤销该槽位，与保存语义一致）。
    pub fn queue_insert_slot(&mut self, offset: usize) {
        self.queue.insert_slot(offset);
        self.queue_begin_edit();
    }

    /// 保存就地编辑：写回槽位；空文本删除槽位（oil.nvim 空行忽略
    /// 语义）。队列清空时退出 QUEUE 回 NORMAL。
    pub fn queue_save_edit(&mut self) {
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
    pub fn enter_queue(&mut self) {
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
    pub fn leave_queue(&mut self) -> Vec<Effect> {
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
    pub fn queue_mode_active(&self) -> bool {
        self.mode == Mode::Queue
    }

    /// 输入框队列区展示行数：各条目逻辑行数之和
    ///（就地编辑的槽位按草稿缓冲行数计）。
    pub fn queue_display_lines(&self) -> u16 {
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
    pub fn execute_slash(&mut self, action: SlashAction) -> Vec<Effect> {
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
    pub fn finish_run(&mut self, notice: Option<String>) {
        self.running = false;
        self.notice = notice;
    }

    /// 开始一轮自动运行（goal 模式追问）：与 prompt 提交同一口径
    /// 先置 running，避免 AgentStart 事件到达前的空窗期重复提交。
    pub fn begin_run(&mut self) {
        self.running = true;
        self.notice = None;
    }

    /// 置状态栏一次性提示（告警等）。
    pub fn warn(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    // ── 会话操作 ────────────────────────────────────────────────────────────

    /// `/skill`：刷新命令行补全快照并列出可用 skill（本地展示，不进上下文）。
    pub fn show_skills(&mut self, skills: Vec<SkillEntry>) {
        self.chat.push_system(skill_list_text(&skills));
        self.command.set_available_skills(skills);
    }

    /// `/new`：清空聊天区开启新对话；session 切换由调用方随后经
    /// [`Self::set_session`] / [`Self::warn`] 回报。
    /// 排队消息属于旧对话的后续意图，随上下文一起清空。
    pub fn start_new_conversation(&mut self) {
        self.chat.clear_items();
        self.queue.clear();
        self.context_tokens = 0;
        self.chat.push_system("已开启新对话，上下文已清空。");
    }

    /// 切换当前 session 标识（`/new` 新建或 `/resume` 恢复后）。
    pub fn set_session(&mut self, session_id: String) {
        self.session_id = Some(session_id);
    }

    /// `/resume`：以恢复的历史消息替换聊天区并切换 session。
    /// 排队消息属于切换前对话的后续意图，随上下文一起清空。
    /// picker 确认后底层模式是 NORMAL（命令受理即回 NORMAL），游标需
    /// 立即定位到最新一条消息，否则 `v`/`yy` 报「没有可选择的消息」。
    pub fn restore_conversation(&mut self, messages: &[Message], session_id: String) {
        self.chat.clear_items();
        self.queue.clear();
        self.load_history(messages);
        self.chat.move_cursor_to_last_message();
        self.session_id = Some(session_id);
    }

    /// `/tree` 选择器确认：以分支重放的消息替换聊天区（session 不变；
    /// 落库父指针切换由调用方随后完成）。
    /// 排队消息属于切换前分支的后续意图，随上下文一起清空。
    pub fn restore_branch(&mut self, messages: &[Message]) {
        self.chat.clear_items();
        self.queue.clear();
        self.load_history(messages);
        self.chat.move_cursor_to_last_message();
    }

    // ── 粘贴与外部编辑器 ────────────────────────────────────────────────────

    /// 粘贴一段文本（可含换行；`\r\n` 统一为 `\n`），随后重算补全。
    pub fn paste_text(&mut self, text: &str) {
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
    pub fn apply_editor_result(&mut self, text: &str) {
        if !self.input.apply_editor_result(text) {
            self.notice = Some("编辑器内容为空，输入保留未变".to_string());
        }
    }

    // ── 选择器（/resume、/models、/tree 共用） ──────────────────────────────

    /// 打开 `/resume` 选择器（从头选中）；调用方保证候选非空。
    pub fn open_resume_picker(&mut self, rows: Vec<PickerRow>) {
        self.picker = Some(Picker::resume(rows));
    }

    /// 打开 `/models` 选择器，预选中当前模型；调用方保证候选非空。
    pub fn open_model_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        self.picker = Some(Picker::models(rows, selected));
    }

    /// 打开思考级别选择器（模型切换流程第二步，预选中当前级别）；
    /// 调用方保证候选非空。
    pub fn open_reasoning_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        self.picker = Some(Picker::reasoning(rows, selected));
    }

    /// 打开 `/tree` 选择器（预选中 `selected`，通常是当前分支末端）；
    /// 调用方保证候选非空且 `selected` 落在可选行上。
    pub fn open_tree_picker(&mut self, rows: Vec<PickerRow>, selected: usize) {
        self.picker = Some(Picker::tree(rows, selected));
    }

    /// 当前选择器（渲染与键位路由用）。
    pub const fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    /// Enter 确认：取出选中行的（种类, id）并关闭选择器。
    /// 过滤后无可见行或选中不可选行（`/tree` 的工具调用条目）时不确认、
    /// 保持打开。
    pub fn take_picker_selection(&mut self) -> Option<(PickerKind, String)> {
        let entry = self.picker.as_ref()?.selected_entry()?;
        self.picker = None;
        Some(entry)
    }

    /// 会话命令（NORMAL `s`/`b`/`c` 直达）：恢复/分支树/新建都是
    /// 会话命令，运行中拒绝并提示（与 COMMAND 下会话命令同一口径）。
    pub fn session_command(&mut self, effect: Effect) -> Vec<Effect> {
        if self.running {
            self.notice = Some("运行中：会话命令（恢复/新建/分支树）须等本轮结束".to_string());
            return Vec::new();
        }
        self.notice = None;
        vec![effect]
    }

    // ── spinner ─────────────────────────────────────────────────────────────

    /// 推进 spinner 一帧（事件循环在运行中周期调用）。
    pub const fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    /// 当前 spinner 帧字符。
    pub const fn spinner(&self) -> &'static str {
        SPINNER_FRAMES[self.spinner % SPINNER_FRAMES.len()]
    }

    // ── 键位帮助弹层 ────────────────────────────────────────────────────────

    /// 键位帮助弹层是否打开（渲染用）。
    pub const fn help_open(&self) -> bool {
        self.help_scroll.is_some()
    }

    /// 帮助弹层滚动状态（渲染时由帮助 widget 钳制回写；打开期间为 `Some`）。
    pub const fn help_scroll_mut(&mut self) -> Option<&mut u16> {
        self.help_scroll.as_mut()
    }

    // ── 复制菜单 ────────────────────────────────────────────────────────────

    /// 当前复制菜单（渲染用）。
    pub const fn copy_menu(&self) -> Option<&CopyMenu> {
        self.copy_menu.as_ref()
    }

    // ── 渲染读接口 ──────────────────────────────────────────────────────────

    /// 是否有 agent 运行在途（spinner 动画与运行态渲染用）。
    pub const fn is_running(&self) -> bool {
        self.running
    }

    /// 是否请求退出（事件循环退出条件）。
    pub const fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// thinking 内容是否折叠显示（`/thinking` 切换）。
    pub const fn thinking_collapsed(&self) -> bool {
        self.thinking_collapsed
    }

    /// goal 模式是否开启（`/goal` 开关，默认关闭）。
    pub const fn goal_mode(&self) -> bool {
        self.goal_mode
    }

    /// 模型展示名。
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// `/models` 切换成功后更新状态栏的模型徽标与上下文窗口。
    pub fn set_model(&mut self, name: String, context_window: u64) {
        self.model_name = name;
        self.context_window = context_window;
    }

    /// 当前 session id（未持久化时为 None；内部标识，不对用户展示）。
    #[cfg(test)]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// 状态栏：当前上下文 token 估算。
    pub const fn context_tokens(&self) -> u64 {
        self.context_tokens
    }

    /// 状态栏：模型上下文窗口（0 = 规格未知）。
    pub const fn context_window(&self) -> u64 {
        self.context_window
    }

    /// 测试辅助：直接设定上下文用量（生产路径只抄事件携带的权威值）。
    #[cfg(test)]
    pub const fn set_context_tokens(&mut self, tokens: u64) {
        self.context_tokens = tokens;
    }

    /// 状态栏一次性提示。
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
}
