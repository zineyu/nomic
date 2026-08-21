//! 提问弹层状态（`ask_user_question` 的用户交互端，ADR-0029）。
//!
//! 模态弹层，键位全接管（与 Picker/Help 同构，[`super::Mode::Question`]
//! 是派生态）。两种阶段：
//! - 选项列表（单选/多选）：↑/↓ 移动，单选 Enter 提交（自定义选项先
//!   输入文本），多选 Space 勾选、Enter 提交；
//! - 自定义输入（单选/多选的「✏️ 其他」或填空）：普通文本编辑，
//!   Enter 提交，Esc 放弃回列表。
//!
//! 答案组装（单选/多选/填空 → [`nomic_tools::AskUserAnswer`]）收在本模块，
//! 提交时经 [`super::Effect::SubmitQuestionAnswer`] 交给事件循环回传工具；
//! Esc 取消经 [`super::Effect::CancelQuestion`] 丢弃注册表条目，工具侧收到
//! 通道关闭转为错误结果回喂模型。

use nomic_tools::{AskUserAnswer, AskUserQuestion, CUSTOM_OPTION, QuestionKind};

use super::{App, Effect, Input, Key};

/// 提问弹层阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuestionPhase {
    /// 选项列表（单选/多选）
    List,
    /// 自定义输入（单选/多选的「✏️ 其他」或填空）
    CustomInput,
}

/// 提问弹层状态：问题 + 列表游标 + 多选勾选 + 自定义输入缓冲。
#[derive(Debug)]
pub(in crate::tui) struct Question {
    /// 完整问题（`options` 末尾为自动追加的自定义选项）
    pub(in crate::tui) prompt: AskUserQuestion,
    /// 列表游标（高亮选项下标）
    pub(in crate::tui) cursor: usize,
    /// 多选已勾选的选项下标（不含自定义选项）
    pub(in crate::tui) selections: Vec<usize>,
    /// 多选的自定义选项是否已勾选（勾选即已输入文本）
    pub(in crate::tui) custom_selected: bool,
    /// 自定义输入缓冲（单选/多选的「✏️ 其他」与填空共用）
    pub(in crate::tui) custom: Input,
    /// 当前阶段
    phase: QuestionPhase,
}

impl Question {
    /// 打开一次提问（游标在首项，无预选；填空直接进入自定义输入）。
    pub(super) fn new(prompt: AskUserQuestion) -> Self {
        let mut custom = Input::new();
        // 自定义输入是纯文本域：不启用命令补全与 `@` mention 补全
        custom.set_completion_enabled(false);
        custom.set_mention_enabled(false);
        // 填空没有候选选项：直接进入自定义输入阶段
        let phase = if prompt.kind == QuestionKind::FillIn {
            QuestionPhase::CustomInput
        } else {
            QuestionPhase::List
        };
        Self {
            prompt,
            cursor: 0,
            selections: Vec::new(),
            custom_selected: false,
            custom,
            phase,
        }
    }

    /// 当前阶段是否为自定义输入（渲染与按键路由用）。
    pub(in crate::tui) const fn is_custom_input(&self) -> bool {
        matches!(self.phase, QuestionPhase::CustomInput)
    }

    /// 自定义选项下标（工具保证追加在末尾；填空无选项返回 `None`）。
    fn custom_index(&self) -> Option<usize> {
        self.prompt
            .options
            .iter()
            .position(|option| option == CUSTOM_OPTION)
    }

    /// 指定下标是否为自定义选项。
    fn is_custom_option(&self, index: usize) -> bool {
        self.custom_index() == Some(index)
    }

    /// 移动列表游标（循环；空列表不动）。
    const fn move_cursor(&mut self, delta: isize) {
        let len = self.prompt.options.len();
        if len == 0 {
            return;
        }
        // 循环移动：下溢回绕到末尾，上溢（不可能）兜底到 0
        self.cursor = match self.cursor.checked_add_signed(delta) {
            Some(next) => next % len,
            None => len - 1,
        };
    }

    /// 勾选/取消勾选一个普通选项（多选）。
    fn toggle(&mut self, index: usize) {
        if let Some(position) = self.selections.iter().position(|&i| i == index) {
            self.selections.remove(position);
        } else {
            self.selections.push(index);
        }
    }

    /// 进入自定义输入阶段：清空缓冲、光标到末尾。
    fn enter_custom_input(&mut self) {
        self.phase = QuestionPhase::CustomInput;
        self.custom.clear_buffer();
        self.custom.cursor_end();
    }

    /// Esc 放弃自定义输入：清空缓冲并取消多选的自选勾选，回到列表。
    fn leave_custom_input(&mut self) {
        self.phase = QuestionPhase::List;
        self.custom.clear_buffer();
        self.custom_selected = false;
    }

    /// 多选提交：勾选选项文本 + 自定义文本（若有），自定义勾选必带文本。
    fn build_multiple_answers(&self) -> AskUserAnswer {
        let mut answers: Vec<String> = self
            .selections
            .iter()
            .map(|&index| self.prompt.options[index].clone())
            .collect();
        let custom = self
            .custom_selected
            .then(|| self.custom.text().trim().to_string());
        if let Some(text) = &custom {
            answers.push(text.clone());
        }
        AskUserAnswer { answers, custom }
    }
}

impl App {
    /// 打开提问弹层（事件循环收到 [`super::super::ask::PendingQuestion`] 时）。
    pub(in crate::tui) fn open_question(&mut self, prompt: AskUserQuestion) {
        self.question = Some(Question::new(prompt));
        self.pending_key = None;
        self.notice = None;
    }

    /// 当前提问弹层（渲染与键位路由用）。
    pub(in crate::tui) const fn question(&self) -> Option<&Question> {
        self.question.as_ref()
    }

    /// 关闭提问弹层（作答 / 取消 / 运行结束兜底）。
    pub(in crate::tui) fn close_question(&mut self) {
        self.question = None;
        self.pending_key = None;
    }

    /// 提问弹层键位（模态接管）：列表与自定义输入两阶段分发。
    pub(super) fn press_question(&mut self, key: Key) -> Vec<Effect> {
        if self
            .question
            .as_ref()
            .is_some_and(Question::is_custom_input)
        {
            self.press_question_input(key)
        } else {
            self.press_question_list(key)
        }
    }

    /// 选项列表阶段：↑/↓（或 j/k）移动；单选 Enter 提交，多选 Space
    /// 勾选、Enter 提交；自定义选项先进入输入阶段；Esc 取消提问。
    fn press_question_list(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Up | Key::Char('k') => {
                if let Some(question) = &mut self.question {
                    question.move_cursor(-1);
                }
            }
            Key::Down | Key::Char('j') => {
                if let Some(question) = &mut self.question {
                    question.move_cursor(1);
                }
            }
            Key::Char(' ') => self.question_space(),
            Key::Enter => return self.question_enter(),
            Key::Esc => return self.question_cancel(),
            // Ctrl+C：退出（中断运行归 NORMAL `q`，与各模式同一口径）
            Key::Ctrl('c') => return self.quit(),
            _ => {}
        }
        Vec::new()
    }

    /// Space：单选无勾选语义（Enter 提交）；多选勾选/取消勾选游标选项，
    /// 自定义选项先进入输入阶段（勾选即带文本）。
    fn question_space(&mut self) {
        let Some(question) = &mut self.question else {
            return;
        };
        if question.prompt.kind != QuestionKind::MultipleChoice {
            return;
        }
        let cursor = question.cursor;
        if question.is_custom_option(cursor) {
            if question.custom_selected {
                question.custom_selected = false;
            } else {
                question.enter_custom_input();
            }
        } else {
            question.toggle(cursor);
        }
    }

    /// 列表 Enter：单选提交游标选项；多选提交全部勾选（空勾选提示
    /// 留在列表）；自定义选项进入输入阶段——多选已勾选（已输入文本）
    /// 的自定义选项按提交处理，不再重复输入。
    fn question_enter(&mut self) -> Vec<Effect> {
        let Some(question) = &mut self.question else {
            return Vec::new();
        };
        let cursor = question.cursor;
        let open_custom_input = question.is_custom_option(cursor)
            && !(question.prompt.kind == QuestionKind::MultipleChoice && question.custom_selected);
        if open_custom_input {
            question.enter_custom_input();
            return Vec::new();
        }
        match question.prompt.kind {
            QuestionKind::SingleChoice => {
                let answer = AskUserAnswer {
                    answers: vec![question.prompt.options[cursor].clone()],
                    custom: None,
                };
                self.question = None;
                vec![Effect::SubmitQuestionAnswer(answer)]
            }
            QuestionKind::MultipleChoice => {
                if question.selections.is_empty() && !question.custom_selected {
                    self.notice = Some("至少勾选一个选项（空格勾选）".to_string());
                    return Vec::new();
                }
                let answer = question.build_multiple_answers();
                self.question = None;
                vec![Effect::SubmitQuestionAnswer(answer)]
            }
            QuestionKind::FillIn => Vec::new(),
        }
    }

    /// 自定义输入阶段：普通文本编辑；Enter 提交（空文本提示留在输入），
    /// Esc 放弃回列表（填空无列表，Esc 直接取消提问）；单行输入不换行、
    /// 无上下导航。
    fn press_question_input(&mut self, key: Key) -> Vec<Effect> {
        match key {
            Key::Enter => return self.question_submit_custom(),
            Key::Esc => {
                let fill_in = self
                    .question
                    .as_ref()
                    .is_some_and(|question| question.prompt.kind == QuestionKind::FillIn);
                if fill_in {
                    return self.question_cancel();
                }
                if let Some(question) = &mut self.question {
                    question.leave_custom_input();
                }
            }
            // 单行输入：Shift+Enter 不换行，↑/↓ 无导航（避免触发聊天区滚动）
            Key::Newline | Key::Up | Key::Down => {}
            // Ctrl+C：退出（与各模式同一口径）
            Key::Ctrl('c') => return self.quit(),
            other => {
                let Some(question) = &mut self.question else {
                    return Vec::new();
                };
                Self::edit_key(&mut question.custom, &mut self.chat, other);
            }
        }
        Vec::new()
    }

    /// 自定义输入 Enter：单选/填空直接提交（答案 = 输入文本）；
    /// 多选勾选自定义选项并回列表（可继续勾选其他选项后 Enter 提交）。
    fn question_submit_custom(&mut self) -> Vec<Effect> {
        let Some(question) = &mut self.question else {
            return Vec::new();
        };
        let text = question.custom.text().trim().to_string();
        if text.is_empty() {
            self.notice = Some("自定义答案不能为空".to_string());
            return Vec::new();
        }
        match question.prompt.kind {
            QuestionKind::SingleChoice | QuestionKind::FillIn => {
                let answer = AskUserAnswer {
                    answers: vec![text.clone()],
                    custom: Some(text),
                };
                self.question = None;
                vec![Effect::SubmitQuestionAnswer(answer)]
            }
            QuestionKind::MultipleChoice => {
                question.custom_selected = true;
                question.phase = QuestionPhase::List;
                Vec::new()
            }
        }
    }

    /// Esc 取消提问：关闭弹层，事件循环丢弃注册表条目（工具转错误结果）。
    fn question_cancel(&mut self) -> Vec<Effect> {
        self.question = None;
        self.pending_key = None;
        vec![Effect::CancelQuestion]
    }
}

#[cfg(test)]
mod tests {
    use super::super::Mode;
    use super::*;

    fn prompt(kind: QuestionKind, options: &[&str]) -> AskUserQuestion {
        AskUserQuestion {
            question: "问题".to_string(),
            kind,
            options: options.iter().copied().map(str::to_string).collect(),
        }
    }

    /// 单选问题（含工具自动追加的自定义选项）。
    fn single_prompt() -> AskUserQuestion {
        prompt(QuestionKind::SingleChoice, &["Rust", "Go", CUSTOM_OPTION])
    }

    fn app() -> App {
        App::new("test-model".to_string(), None, 200_000)
    }

    fn open(app: &mut App, prompt: AskUserQuestion) {
        app.open_question(prompt);
        assert_eq!(app.mode(), Mode::Question);
    }

    #[test]
    fn single_choice_enter_submits_highlighted_option() {
        let mut app = app();
        open(&mut app, single_prompt());
        // 下移一次到「Go」再提交
        app.press(Key::Down);
        let effects = app.press(Key::Enter);
        let [Effect::SubmitQuestionAnswer(answer)] = effects.as_slice() else {
            panic!("expected submit effect: {effects:?}");
        };
        assert_eq!(answer.answers, ["Go"]);
        assert_eq!(answer.custom, None);
        assert_eq!(app.mode(), Mode::Insert, "提交后弹层关闭");
    }

    #[test]
    fn single_choice_custom_option_opens_input_then_submits() {
        let mut app = app();
        open(&mut app, single_prompt());
        // 移到自定义选项（下标 2）进入输入
        for _ in 0..2 {
            app.press(Key::Down);
        }
        let effects = app.press(Key::Enter);
        assert!(effects.is_empty(), "自定义选项先进入输入阶段");
        assert!(app.question().is_some_and(Question::is_custom_input));

        app.paste_text("Python");
        let effects = app.press(Key::Enter);
        let [Effect::SubmitQuestionAnswer(answer)] = effects.as_slice() else {
            panic!("expected submit effect: {effects:?}");
        };
        assert_eq!(answer.answers, ["Python"]);
        assert_eq!(answer.custom.as_deref(), Some("Python"));
    }

    #[test]
    fn single_choice_empty_custom_rejected() {
        let mut app = app();
        open(&mut app, single_prompt());
        for _ in 0..2 {
            app.press(Key::Down);
        }
        app.press(Key::Enter);
        app.press(Key::Enter);
        assert_eq!(app.mode(), Mode::Question, "空文本留在输入阶段");
        assert!(app.notice().is_some_and(|n| n.contains("不能为空")));
    }

    #[test]
    fn multiple_choice_space_toggles_and_enter_submits() {
        let mut app = app();
        open(
            &mut app,
            prompt(
                QuestionKind::MultipleChoice,
                &["A", "B", "C", CUSTOM_OPTION],
            ),
        );
        // 勾选 A（游标在 0）
        app.press(Key::Char(' '));
        // 移到 B 勾选
        app.press(Key::Down);
        app.press(Key::Char(' '));
        let effects = app.press(Key::Enter);
        let [Effect::SubmitQuestionAnswer(answer)] = effects.as_slice() else {
            panic!("expected submit effect: {effects:?}");
        };
        assert_eq!(answer.answers, ["A", "B"]);
        assert_eq!(answer.custom, None);
    }

    #[test]
    fn multiple_choice_custom_flow() {
        let mut app = app();
        open(
            &mut app,
            prompt(QuestionKind::MultipleChoice, &["A", "B", CUSTOM_OPTION]),
        );
        // 勾选 A
        app.press(Key::Char(' '));
        // 移到自定义选项并输入文本
        app.press(Key::Down);
        app.press(Key::Down);
        app.press(Key::Enter);
        assert!(app.question().is_some_and(Question::is_custom_input));
        app.paste_text("其它方案");
        app.press(Key::Enter);
        assert_eq!(app.mode(), Mode::Question, "多选自定义后回列表");
        assert!(!app.question().is_some_and(Question::is_custom_input));

        let effects = app.press(Key::Enter);
        let [Effect::SubmitQuestionAnswer(answer)] = effects.as_slice() else {
            panic!("expected submit effect: {effects:?}");
        };
        assert_eq!(answer.answers, ["A", "其它方案"]);
        assert_eq!(answer.custom.as_deref(), Some("其它方案"));
    }

    #[test]
    fn multiple_choice_empty_submit_hints() {
        let mut app = app();
        open(
            &mut app,
            prompt(QuestionKind::MultipleChoice, &["A", "B", CUSTOM_OPTION]),
        );
        app.press(Key::Enter);
        assert_eq!(app.mode(), Mode::Question, "空勾选留在列表");
        assert!(app.notice().is_some_and(|n| n.contains("至少勾选")));
    }

    #[test]
    fn esc_cancels_question() {
        let mut app = app();
        open(&mut app, single_prompt());
        let effects = app.press(Key::Esc);
        let [Effect::CancelQuestion] = effects.as_slice() else {
            panic!("expected cancel effect: {effects:?}");
        };
        assert_eq!(app.mode(), Mode::Insert);
        assert!(app.question().is_none());
    }

    #[test]
    fn esc_in_custom_input_returns_to_list_and_clears() {
        let mut app = app();
        open(
            &mut app,
            prompt(QuestionKind::MultipleChoice, &["A", CUSTOM_OPTION]),
        );
        app.press(Key::Down);
        app.press(Key::Enter);
        app.paste_text("临时文本");
        app.press(Key::Esc);
        assert!(!app.question().is_some_and(Question::is_custom_input));
        // 再次进入自定义输入应为空缓冲
        app.press(Key::Enter);
        assert!(app.question().is_some_and(Question::is_custom_input));
        assert!(app.question().unwrap().custom.text().is_empty());
    }

    #[test]
    fn fill_in_edits_and_submits_text() {
        let mut app = app();
        open(&mut app, prompt(QuestionKind::FillIn, &[]));
        // 填空直接进入自定义输入阶段
        assert!(app.question().is_some_and(Question::is_custom_input));
        app.paste_text("a@b.c");
        let effects = app.press(Key::Enter);
        let [Effect::SubmitQuestionAnswer(answer)] = effects.as_slice() else {
            panic!("expected submit effect: {effects:?}");
        };
        assert_eq!(answer.answers, ["a@b.c"]);
        assert_eq!(answer.custom.as_deref(), Some("a@b.c"));
    }

    #[test]
    fn finish_run_closes_question_modal() {
        let mut app = app();
        open(&mut app, single_prompt());
        app.finish_run(Some("运行结束".to_string()));
        assert!(app.question().is_none());
        assert_eq!(app.mode(), Mode::Insert);
    }
}
