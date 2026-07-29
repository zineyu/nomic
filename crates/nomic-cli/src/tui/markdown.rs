//! assistant 文本块的 Markdown 渲染：pulldown-cmark 解析 → 带主题样式的 ratatui 行。
//!
//! 覆盖 agent 输出的常见结构：标题、加粗/斜体/删除线、行内代码与代码块、
//! 有序/无序列表（含嵌套）、任务列表、引用块、链接、表格、分割线；
//! 其余结构（HTML、脚注定义等）退化为纯文本或忽略。
//!
//! 流式增量（未闭合的 fence、半截标记）由 pulldown-cmark 容错解析，
//! 按已收到的前缀渲染即可，无需特殊处理；调用方按宽度折行（见 `ui::wrap_lines`）。

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::theme;

/// 把 Markdown 文本渲染为带样式的逻辑行（未折行）。
///
/// `width` 仅用于分割线 / 表头分隔线这类占满整行的装饰。
pub(super) fn render(text: &str, width: u16) -> Vec<Line<'static>> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut renderer = Renderer::new(width);
    for event in Parser::new_ext(text, options) {
        renderer.event(&event);
    }
    renderer.finish()
}

/// 表格行状态：表头行单元格加粗；`cell_started` 决定是否补 ` │ ` 分隔。
struct TableState {
    head: bool,
    cell_started: bool,
}

/// 事件流 → 行的增量构建器。
struct Renderer {
    /// 已定稿的行
    lines: Vec<Line<'static>>,
    /// 当前行的 span（新行起始为空，行首前缀在首个内容写入时套用）
    spans: Vec<Span<'static>>,
    /// 行首前缀栈（引用竖条、列表续行缩进），每层块结构进/出时 push/pop
    prefixes: Vec<Span<'static>>,
    /// 当前行的前缀是否已写入 `spans`
    prefix_applied: bool,
    /// 内联样式栈（标题/加粗/斜体/链接等，可嵌套叠加）
    styles: Vec<Style>,
    /// 列表栈：`Some(n)` 为有序列表的下一编号，`None` 为无序
    lists: Vec<Option<u64>>,
    /// 代码块内（Text 事件按代码样式渲染）
    in_code_block: bool,
    /// 表格行状态（表格外为 `None`）
    table: Option<TableState>,
    /// 列表项符号刚落行、正文未到（项内首个 paragraph 不做块间空行）
    item_marker_pending: bool,
    /// 分割线 / 表头分隔线的目标宽度
    width: u16,
}

impl Renderer {
    const fn new(width: u16) -> Self {
        Self {
            lines: Vec::new(),
            spans: Vec::new(),
            prefixes: Vec::new(),
            prefix_applied: false,
            styles: Vec::new(),
            lists: Vec::new(),
            in_code_block: false,
            table: None,
            item_marker_pending: false,
            width,
        }
    }

    fn event(&mut self, event: &Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(*tag),
            Event::Text(text) => {
                let style = if self.in_code_block {
                    theme::code()
                } else {
                    self.current_style()
                };
                self.push_text(text, style);
            }
            Event::Code(code) => {
                let style = self.current_style().patch(theme::code());
                self.push_span(code.to_string(), style);
            }
            Event::SoftBreak | Event::HardBreak => self.new_line(),
            Event::Rule => {
                self.block_separation();
                let rule = "─".repeat(usize::from(self.width).max(1));
                self.push_span(rule, theme::dim());
                self.new_line();
            }
            Event::TaskListMarker(checked) => {
                let marker = if *checked { "☑ " } else { "☐ " };
                self.push_span(marker.to_string(), theme::dim());
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                self.push_text(html, theme::dim());
            }
            Event::FootnoteReference(name) => {
                self.push_span(format!("[^{name}]"), theme::dim());
            }
            // InlineMath/DisplayMath 未启用对应扩展，不会出现
            _ => {}
        }
    }

    fn start_tag(&mut self, tag: &Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                // 列表项符号后的首个 paragraph 与符号同行，不做块间空行
                if !self.item_marker_pending {
                    self.block_separation();
                }
            }
            Tag::Heading { .. } => {
                self.block_separation();
                self.styles.push(theme::heading());
            }
            Tag::BlockQuote(_) => {
                self.block_separation();
                self.prefixes
                    .push(Span::styled("▎ ".to_string(), theme::dim()));
            }
            Tag::CodeBlock(_) => {
                self.block_separation();
                self.prefixes.push(Span::raw("  ".to_string()));
                self.in_code_block = true;
            }
            Tag::List(start) => {
                // 顶层列表与上文空行分隔；嵌套列表紧跟父项内容不空行
                if self.lists.is_empty() {
                    self.block_separation();
                } else {
                    self.flush_line();
                }
                self.lists.push(*start);
            }
            Tag::Item => {
                self.flush_line();
                self.ensure_prefix();
                let marker = match self.lists.last_mut() {
                    Some(Some(next)) => {
                        let marker = format!("{next}. ");
                        *next += 1;
                        marker
                    }
                    _ => "• ".to_string(),
                };
                let continuation = " ".repeat(marker.chars().count());
                self.spans.push(Span::styled(marker, theme::dim()));
                self.prefixes.push(Span::raw(continuation));
                self.item_marker_pending = true;
            }
            Tag::Emphasis => self.styles.push(theme::italic()),
            Tag::Strong => self.styles.push(theme::bold()),
            Tag::Strikethrough => self.styles.push(theme::strikethrough()),
            Tag::Link { .. } => self.styles.push(theme::link()),
            Tag::Image { .. } => self.styles.push(theme::dim()),
            Tag::Table(_) => {
                self.block_separation();
            }
            Tag::TableHead | Tag::TableRow => {
                self.flush_line();
                self.table = Some(TableState {
                    head: matches!(tag, Tag::TableHead),
                    cell_started: false,
                });
            }
            Tag::TableCell => {
                let (cell_started, head) = self
                    .table
                    .as_ref()
                    .map_or((false, false), |table| (table.cell_started, table.head));
                if cell_started {
                    self.push_span(" │ ".to_string(), theme::dim());
                }
                if head {
                    self.styles.push(theme::bold());
                }
                if let Some(table) = &mut self.table {
                    table.cell_started = true;
                }
            }
            // FootnoteDefinition/MetadataBlock/HtmlBlock/DefinitionList 等：内容按纯文本流过
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Heading(_) | TagEnd::Table => {
                self.table = None;
                self.flush_line();
            }
            TagEnd::BlockQuote(_) => {
                self.prefixes.pop();
                self.flush_line();
            }
            TagEnd::CodeBlock => {
                self.prefixes.pop();
                self.in_code_block = false;
                self.flush_line();
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.flush_line();
            }
            TagEnd::Item => {
                self.prefixes.pop();
                self.item_marker_pending = false;
            }
            TagEnd::TableHead => {
                self.flush_line();
                self.table = None;
                let rule = "─".repeat(usize::from(self.width).max(1));
                self.push_span(rule, theme::dim());
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    table.cell_started = false;
                }
            }
            TagEnd::TableCell => {
                if self.table.as_ref().is_some_and(|table| table.head) {
                    self.styles.pop();
                }
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Link
            | TagEnd::Image => {
                self.styles.pop();
            }
            _ => {}
        }
    }

    /// 结束构建：冲刷末行、去掉尾部空行。
    fn finish(mut self) -> Vec<Line<'static>> {
        self.new_line();
        while self.lines.last().is_some_and(|line| is_blank(line)) {
            self.lines.pop();
        }
        self.lines
    }

    /// 当前行落盘，开始新行（前缀延迟到首个内容写入时套用）。
    ///
    /// 用于文本内的真实换行（SoftBreak/HardBreak/代码块内空行）：
    /// 待冲刷内容为空且上一行已是空行（或尚无任何行）时跳过，折叠连续空行。
    fn new_line(&mut self) {
        if self.spans.is_empty() && self.lines.last().is_none_or(|line| is_blank(line)) {
            return;
        }
        self.lines.push(Line::from(std::mem::take(&mut self.spans)));
        self.prefix_applied = false;
    }

    /// 块结构边界的预防性落盘：仅落盘非空白内容，不制造空行。
    fn flush_line(&mut self) {
        if self.spans.iter().all(|span| span.content.trim().is_empty()) {
            self.spans.clear();
        } else {
            self.lines.push(Line::from(std::mem::take(&mut self.spans)));
        }
        self.prefix_applied = false;
    }

    /// 块级元素之间的分隔：冲刷当前行，且与上文之间恰好空一行。
    fn block_separation(&mut self) {
        self.flush_line();
        while self.lines.last().is_some_and(|line| is_blank(line)) {
            self.lines.pop();
        }
        if !self.lines.is_empty() {
            self.lines.push(Line::default());
        }
    }

    /// 写入首个内容前补行首前缀（引用竖条、列表续行缩进）。
    fn ensure_prefix(&mut self) {
        if !self.prefix_applied {
            self.spans.extend(self.prefixes.iter().cloned());
            self.prefix_applied = true;
        }
    }

    /// 追加单个 span 内容。
    fn push_span(&mut self, content: String, style: Style) {
        if content.is_empty() {
            return;
        }
        self.ensure_prefix();
        self.item_marker_pending = false;
        self.spans.push(Span::styled(content, style));
    }

    /// 追加文本（可能含换行，如代码块），按 `\n` 拆到多行。
    fn push_text(&mut self, text: &str, style: Style) {
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.new_line();
            }
            if !part.is_empty() {
                self.push_span(part.to_string(), style);
            }
        }
    }

    /// 内联样式栈叠加结果（后入栈的 patch 覆盖先入栈的）。
    fn current_style(&self) -> Style {
        self.styles
            .iter()
            .fold(Style::default(), |style, patch| style.patch(*patch))
    }
}

/// 行是否无可见内容（前缀缩进等纯空白视为空行）。
fn is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use super::*;

    /// 把渲染结果压成纯文本便于断言。
    fn plain(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn heading_is_bold_accent_and_blocks_are_separated() {
        let lines = render("# 标题\n\n正文段落。\n\n## 小节", 80);
        let text = plain(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows, ["标题", "", "正文段落。", "", "小节"]);
        let heading = &lines[0].spans[0];
        assert_eq!(heading.style.fg, Some(theme::ACCENT));
        assert!(heading.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn unordered_and_ordered_lists_get_markers_and_continuation_indent() {
        let lines = render("- 甲\n- 乙\n\n1. 一\n2. 二", 80);
        let text = plain(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows, ["• 甲", "• 乙", "", "1. 一", "2. 二"]);
    }

    #[test]
    fn nested_list_uses_continuation_prefix() {
        let lines = render("- 外层\n  - 内层", 80);
        let text = plain(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows, ["• 外层", "  • 内层"]);
    }

    #[test]
    fn code_block_is_indented_and_code_styled() {
        let lines = render("```rust\nfn main() {}\n```", 80);
        assert_eq!(lines.len(), 1);
        let text = plain(&lines);
        assert_eq!(text, "  fn main() {}");
        let code = &lines[0].spans[1];
        assert_eq!(code.style.fg, Some(theme::CODE));
    }

    #[test]
    fn unclosed_code_fence_renders_prefix_during_streaming() {
        let lines = render("```\npartial line", 80);
        assert_eq!(plain(&lines), "  partial line");
    }

    #[test]
    fn block_quote_gets_bar_prefix() {
        let lines = render("> 引用内容", 80);
        assert_eq!(plain(&lines), "▎ 引用内容");
    }

    #[test]
    fn inline_styles_compose() {
        let lines = render("普通 **加粗** *斜体* ~~删除~~ `代码`", 80);
        let text = plain(&lines);
        assert_eq!(text, "普通 加粗 斜体 删除 代码");
        let spans = &lines[0].spans;
        let bold = spans
            .iter()
            .find(|s| s.content == "加粗")
            .expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let italic = spans
            .iter()
            .find(|s| s.content == "斜体")
            .expect("italic span");
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
        let strike = spans
            .iter()
            .find(|s| s.content == "删除")
            .expect("strike span");
        assert!(strike.style.add_modifier.contains(Modifier::CROSSED_OUT));
        let code = spans
            .iter()
            .find(|s| s.content == "代码")
            .expect("code span");
        assert_eq!(code.style.fg, Some(theme::CODE));
    }

    #[test]
    fn link_text_is_underlined() {
        let lines = render("[文档](https://example.com)", 80);
        let link = &lines[0].spans[0];
        assert_eq!(link.content.as_ref(), "文档");
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn table_renders_cells_with_separator_and_header_rule() {
        let lines = render("| 名称 | 值 |\n| --- | --- |\n| a | 1 |", 80);
        let text = plain(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], "名称 │ 值");
        assert!(rows[1].chars().all(|c| c == '─'), "{rows:?}");
        assert_eq!(rows[2], "a │ 1");
        // 表头加粗
        let head = &lines[0].spans[0];
        assert!(head.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn task_list_marker_renders_checkbox() {
        let lines = render("- [ ] 待办\n- [x] 完成", 80);
        let text = plain(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows, ["• ☐ 待办", "• ☑ 完成"]);
    }

    #[test]
    fn rule_fills_width() {
        let lines = render("上文\n\n---\n\n下文", 40);
        let text = plain(&lines);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows, ["上文", "", "─".repeat(40).as_str(), "", "下文"]);
    }

    #[test]
    fn plain_text_round_trips_without_extra_blank_lines() {
        let lines = render("第一行\n第二行\n", 80);
        let text = plain(&lines);
        assert_eq!(text, "第一行\n第二行");
    }

    #[test]
    fn empty_input_yields_no_lines() {
        assert!(render("", 80).is_empty());
        assert!(render("\n\n", 80).is_empty());
    }
}
