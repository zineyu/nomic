//! 内嵌草稿编辑器：edtui 的薄封装（防腐层）。
//!
//! 只暴露 nomic 需要的协议——打开（`initial`）、按键（保存/放弃/继续）、
//! 渲染、光标查询；edtui 的类型不外泄到 `app`/`ui`/`mod`。
//!
//! 按键协议（最基础 vim 编辑）：
//! - 打开即 INSERT（用户正在起草，`Ctrl+G` 的意图是继续编辑长文），光标在文末
//! - INSERT 下 `Esc` 回 NORMAL（edtui vim 键位）；NORMAL 下 `Esc` 保存并关闭
//! - 任意时刻 `Ctrl+C` 放弃修改（草稿保留）

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use edtui::{
    EditorEventHandler, EditorMode, EditorState, EditorTheme, EditorView, Index2, Lines,
    actions::SwitchMode,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::Widget,
};

/// 按键的语义结果：事件循环据此决定写回或放弃。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DraftAction {
    /// 编辑器已消费按键，继续编辑
    Continue,
    /// NORMAL 下 `Esc`：保存编辑内容并关闭
    Save(String),
    /// `Ctrl+C`：放弃修改并关闭（原草稿不动）
    Cancel,
}

/// 草稿编辑器状态：edtui 的 `EditorState` + vim 键位 handler。
pub(super) struct DraftEditor {
    state: EditorState,
    handler: EditorEventHandler,
}

// edtui 类型未实现 Debug；App 派生 Debug 需要，手工给出版本号式占位
impl std::fmt::Debug for DraftEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftEditor")
            .field("mode", &self.state.mode)
            .finish_non_exhaustive()
    }
}

impl DraftEditor {
    /// 以当前草稿为初始内容打开；进入 INSERT 模式，光标置于文末
    ///（继续输入是最常见意图，与外部编辑器写回后光标在末尾的口径一致）。
    pub(super) fn new(initial: &str) -> Self {
        let mut state = EditorState::new(Lines::from(initial));
        // 先切 INSERT 再定位：Insert 模式下光标可停在行尾后一位
        //（max_col 按模式计算；Normal 下 len-1 会被 clamp_column 钳回）
        state.execute(SwitchMode(EditorMode::Insert));
        let last_row = state.lines.len().saturating_sub(1);
        let last_col = state.lines.len_col(last_row).unwrap_or(0);
        state.cursor = Index2::new(last_row, last_col);
        Self {
            state,
            handler: EditorEventHandler::vim_mode(),
        }
    }

    /// 处理一个终端按键。保存/放弃由本层拦截（edtui 无 `:wq` 语义）；
    /// 其余按键转发 edtui vim 键位。
    pub(super) fn handle_key(&mut self, key: KeyEvent) -> DraftAction {
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            return DraftAction::Cancel;
        }
        // NORMAL 下 Esc 在 edtui vim 键位中无绑定，复用为"保存退出"
        //（与 QUEUE 就地编辑 Enter/Esc 保存的口径一致）
        if key.code == KeyCode::Esc && self.state.mode == EditorMode::Normal {
            return DraftAction::Save(self.state.lines.to_string());
        }
        self.handler.on_key_event(key, &mut self.state);
        DraftAction::Continue
    }

    /// 渲染编辑器（含 edtui 自带模式状态行）。光标屏幕位置在渲染后
    /// 由 [`Self::cursor_position`] 读取。
    pub(super) fn render(&mut self, area: Rect, buf: &mut Buffer) {
        EditorView::new(&mut self.state)
            .theme(theme())
            .wrap(true)
            .render(area, buf);
    }

    /// 光标的绝对终端坐标（渲染后方可知）。
    pub(super) fn cursor_position(&self) -> Option<(u16, u16)> {
        self.state
            .cursor_screen_position()
            .map(|pos| (pos.x, pos.y))
    }

    /// 是否处于可键入态（INSERT）：光标形状用竖条，否则实心块。
    pub(super) fn is_insert(&self) -> bool {
        self.state.mode == EditorMode::Insert
    }
}

/// 编辑器配色：底色跟随终端默认（不硬编码黑底白字），选择区用反色
/// 之外的低饱和度色，避免与 nomic 主题色相冲突。
fn theme() -> EditorTheme<'static> {
    EditorTheme::default()
        .base(Style::default())
        .cursor_style(Style::default().fg(Color::Black).bg(Color::White))
        .selection_style(Style::default().fg(Color::Black).bg(Color::Yellow))
        .line_numbers_style(Style::default().fg(Color::DarkGray))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_str(editor: &mut DraftEditor, text: &str) {
        for c in text.chars() {
            let action = editor.handle_key(key(KeyCode::Char(c)));
            assert_eq!(action, DraftAction::Continue);
        }
    }

    #[test]
    fn open_enters_insert_with_cursor_at_end() {
        let mut editor = DraftEditor::new("第一行\n第二行");
        assert!(editor.is_insert());
        // 光标在文末：直接输入应追加到最后一行
        type_str(&mut editor, "尾");
        // 第一个 Esc 回 NORMAL，第二个触发保存
        assert_eq!(editor.handle_key(key(KeyCode::Esc)), DraftAction::Continue);
        assert_eq!(
            editor.handle_key(key(KeyCode::Esc)),
            DraftAction::Save("第一行\n第二行尾".to_string())
        );
    }

    #[test]
    fn esc_in_insert_returns_to_normal_then_saves() {
        let mut editor = DraftEditor::new("draft");
        type_str(&mut editor, "+");
        assert_eq!(editor.handle_key(key(KeyCode::Esc)), DraftAction::Continue);
        assert!(!editor.is_insert());
        assert_eq!(
            editor.handle_key(key(KeyCode::Esc)),
            DraftAction::Save("draft+".to_string())
        );
    }

    #[test]
    fn ctrl_c_cancels_in_any_mode() {
        let mut editor = DraftEditor::new("draft");
        assert_eq!(editor.handle_key(ctrl('c')), DraftAction::Cancel);

        let mut editor = DraftEditor::new("draft");
        editor.handle_key(key(KeyCode::Esc)); // 回 NORMAL
        assert_eq!(editor.handle_key(ctrl('c')), DraftAction::Cancel);
    }

    #[test]
    fn normal_mode_basic_vim_editing() {
        let mut editor = DraftEditor::new("hello world");
        editor.handle_key(key(KeyCode::Esc)); // NORMAL，光标在行尾
        // `b` 回词首，`x` 删除字符
        editor.handle_key(key(KeyCode::Char('b')));
        editor.handle_key(key(KeyCode::Char('x')));
        assert_eq!(
            editor.handle_key(key(KeyCode::Esc)),
            DraftAction::Save("hello orld".to_string())
        );
    }

    #[test]
    fn multiline_roundtrip() {
        let mut editor = DraftEditor::new("a\nb\nc");
        assert_eq!(editor.handle_key(key(KeyCode::Esc)), DraftAction::Continue);
        assert_eq!(
            editor.handle_key(key(KeyCode::Esc)),
            DraftAction::Save("a\nb\nc".to_string())
        );
    }
}
