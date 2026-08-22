//! 运行状态提示 widget：输入框上方的单行阶段提示（`thinking...` /
//! `tool calling...` / `writing...` / `waiting...`），文字带自左到右
//! 循环扫过的高亮动效。
//!
//! [`RunHint`] 是 [`Widget`]：从 [`App`] 只读构建。阶段由
//! [`App::run_phase`] 按聊天区尾部推导；扫光相位复用 spinner 帧序号
//!（事件循环在运行中每 100ms 推进一次，空闲时本行不占布局也不渲染）。

use ratatui::{
    buffer::Buffer,
    layout::{Margin, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::chat::CHAT_H_MARGIN;
use crate::tui::app::{App, RunPhase};
use crate::tui::theme;

/// 扫光高亮带宽度（字符数）：头部 1 字最亮，其余为拖尾。
const SHIMMER_BAND: usize = 3;

/// 运行状态提示 widget：从 [`App`] 只读构建单行画面。
pub(in crate::tui) struct RunHint<'a> {
    app: &'a App,
}

impl<'a> RunHint<'a> {
    pub(in crate::tui) const fn new(app: &'a App) -> Self {
        Self { app }
    }
}

impl Widget for RunHint<'_> {
    /// 渲染运行提示：`▌` gutter（busy 色，与聊天区组件同一语言）+
    /// 扫光文本。空闲时（`run_phase` 为 `None`）不渲染任何内容。
    fn render(self, area: Rect, buf: &mut Buffer) {
        let Some(phase) = self.app.run_phase() else {
            return;
        };
        let text = phase_text(self.app, phase);
        let mut spans = vec![Span::styled("▌ ", theme::busy())];
        spans.extend(shimmer_spans(&text, self.app.spinner_tick()));
        Paragraph::new(Line::from(spans)).render(area.inner(Margin::new(CHAT_H_MARGIN, 0)), buf);
    }
}

/// 各阶段的提示文案（同一风格：小写动词 + 省略号）；工具阶段标注工具名。
fn phase_text(app: &App, phase: RunPhase) -> String {
    match phase {
        RunPhase::Waiting => "waiting...".to_string(),
        RunPhase::Thinking => "thinking...".to_string(),
        RunPhase::Writing => "writing...".to_string(),
        RunPhase::ToolCalling => app.running_tool().map_or_else(
            || "tool calling...".to_string(),
            |tool| format!("tool calling({})...", tool.name),
        ),
    }
}

/// 扫光文本 spans：高亮带（宽 [`SHIMMER_BAND]`）随 `tick` 自左到右移动，
/// 带内头部最亮、拖尾次之，带外为暗色；周期 = 文本长 + 带宽（扫光完全
/// 离开右缘后从头再来）。
fn shimmer_spans(text: &str, tick: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let period = chars.len() + SHIMMER_BAND;
    let head = tick % period;
    chars
        .iter()
        .enumerate()
        .map(|(index, ch)| {
            let style = if index <= head && head - index < SHIMMER_BAND {
                if head == index {
                    theme::shimmer_head()
                } else {
                    theme::shimmer_trail()
                }
            } else {
                theme::dim()
            };
            Span::styled(ch.to_string(), style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 提取 spans 的文本与样式对，便于断言扫光位置。
    fn styled_chars(text: &str, tick: usize) -> Vec<(char, ratatui::style::Style)> {
        shimmer_spans(text, tick)
            .iter()
            .map(|span| {
                (
                    span.content.chars().next().expect("single char span"),
                    span.style,
                )
            })
            .collect()
    }

    /// 扫光带随 tick 自左到右推进：tick=0 时头部在首字符，带外均为暗色；
    /// 扫过右缘后（tick ≥ 文本长）全部落回暗色，周期结束后从头再来。
    #[test]
    fn shimmer_band_moves_left_to_right() {
        let text = "ab";
        let at = |tick| styled_chars(text, tick);

        // tick=0：头部在 'a'，'b' 尚未被扫到
        let spans = at(0);
        assert_eq!(spans[0], ('a', theme::shimmer_head()));
        assert_eq!(spans[1], ('b', theme::dim()));

        // tick=1：头部右移到 'b'，'a' 成为拖尾
        let spans = at(1);
        assert_eq!(spans[0], ('a', theme::shimmer_trail()));
        assert_eq!(spans[1], ('b', theme::shimmer_head()));

        // tick=2/3：扫光带尾部仍压着文本（带宽 3），最后一个字符先头部后拖尾
        assert_eq!(at(2)[1], ('b', theme::shimmer_trail()));
        assert_eq!(at(3)[1], ('b', theme::shimmer_trail()));

        // tick=4：扫光带完全离开右缘（周期 = 2 + 3 = 5），全部暗色
        assert!(
            at(4).iter().all(|(_, style)| *style == theme::dim()),
            "tick=4 扫光应已离开文本"
        );

        // tick=5：周期结束，头部回到首字符
        assert_eq!(at(5)[0], ('a', theme::shimmer_head()));
    }

    /// 提示文案：四阶段同一风格；工具阶段标注运行中的工具名。
    #[test]
    fn phase_texts_share_style() {
        let mut app = App::new("test-model".to_string(), None, 200_000);
        assert_eq!(phase_text(&app, RunPhase::Waiting), "waiting...");
        assert_eq!(phase_text(&app, RunPhase::Thinking), "thinking...");
        assert_eq!(phase_text(&app, RunPhase::Writing), "writing...");
        // 无运行中工具时退化为通用文案
        assert_eq!(phase_text(&app, RunPhase::ToolCalling), "tool calling...");

        app.handle_event(&nomic_core::AgentEvent::ToolExecutionStart {
            tool_call_id: "t1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({}),
        });
        assert_eq!(
            phase_text(&app, RunPhase::ToolCalling),
            "tool calling(bash)..."
        );
    }
}
