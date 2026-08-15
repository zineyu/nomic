//! 输入区状态：缓冲、编辑与命令补全。
//!
//! [`Input`] 自持文本缓冲、光标、补全弹层、暂存附件与补全快照
//! （skill/template）；提交、模式切换与效果分发由 [`super::App`] 路由。
//! `App` 持有两份：聊天草稿（INSERT/QUEUE 就地编辑，补全不启用）与
//! 命令栏（COMMAND 模式，命令补全常驻启用，ADR-0020）。

use nomic_prompts::PromptTemplate;
use nomic_skills::SkillScope;
use unicode_width::UnicodeWidthStr;

use super::{COMMANDS, Command, line_count_of};
use crate::tui::mention;

/// 补全候选：内建命令、prompt template 或 `skill:` 后的 skill 名。
#[derive(Debug)]
pub(in crate::tui) enum CompletionCandidate {
    Command(&'static Command),
    /// prompt template（`name` 调用展开）
    Template(PromptTemplate),
    Skill(SkillEntry),
}

impl CompletionCandidate {
    /// 候选对应的输入片段，用于精确匹配、排序与填入。
    pub(super) fn fragment(&self) -> String {
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

/// 可用于 `skill:` 补全的 skill 元数据（从 resolver catalog 快照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct SkillEntry {
    pub(in crate::tui) name: String,
    pub(in crate::tui) description: String,
    pub(in crate::tui) scope: SkillScope,
}

/// `@` mention 补全弹层状态。
#[derive(Debug)]
pub(in crate::tui) struct MentionCompletion {
    pub(in crate::tui) candidates: Vec<MentionCandidate>,
    pub(in crate::tui) selected: usize,
}

/// `@` mention 的单个补全候选：`fragment` 为填入草稿的完整标记
/// （如 `@skill:jujutsu`），`display` 为弹层展示文本。
#[derive(Debug)]
pub(in crate::tui) struct MentionCandidate {
    pub(in crate::tui) fragment: String,
    pub(in crate::tui) display: String,
}

/// 补全弹层状态：候选列表 + 当前选中项。
#[derive(Debug)]
pub(in crate::tui) struct Completion {
    pub(in crate::tui) candidates: Vec<CompletionCandidate>,
    pub(in crate::tui) selected: usize,
}

/// 暂存的图片附件（`image <路径>` 命令载入，随下一条 prompt 一起发送）。
#[derive(Debug)]
struct PendingImage {
    /// 展示名（文件名）
    name: String,
    /// 图片内容块（base64 内联）
    image: nomic_ai::ImageContent,
}

/// 输入区状态：文本缓冲 + 光标 + 补全 + 附件。编辑操作内部维护补全
/// 弹层；补全仅在 `completion_enabled`（命令栏常驻启用，聊天草稿
/// 与 QUEUE 就地编辑不启用，ADR-0020）时弹出。
#[derive(Debug)]
pub(in crate::tui) struct Input {
    /// 输入缓冲（草稿可多行，`\n` 为 Shift+Enter 插入的手动换行；
    /// 命令栏单行，粘贴的换行由模式路由层折叠为空格）
    pub(super) text: String,
    /// 光标位置（字节索引，始终落在 char 边界）
    pub(super) cursor: usize,
    /// 命令补全弹层（启用补全的缓冲中命令名编辑期间出现）
    completion: Option<Completion>,
    /// `@` mention 补全弹层（草稿输入框以 `@` 收尾时出现）
    mention: Option<MentionCompletion>,
    /// 暂存的图片附件（随下一条 prompt 发送；仅聊天草稿使用）
    attachments: Vec<PendingImage>,
    /// `skill:` 补全用的可用 skill 快照
    skills: Vec<SkillEntry>,
    /// 可用的 prompt templates（`name` 调用展开与补全用）
    templates: Vec<PromptTemplate>,
    /// 补全是否启用：仅命令栏启用（ADR-0020；草稿不承载命令，
    /// QUEUE 就地编辑的是排队消息文本而非命令，均不启用）
    completion_enabled: bool,
    /// `@` mention 补全是否启用：聊天草稿启用，命令栏不启用
    mention_enabled: bool,
}

impl Input {
    pub(super) const fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            completion: None,
            mention: None,
            attachments: Vec::new(),
            skills: Vec::new(),
            templates: Vec::new(),
            completion_enabled: true,
            mention_enabled: true,
        }
    }

    // ── 快照（补全数据源） ──────────────────────────────────────────────────

    /// 设置 `skill:` 补全用的可用 skill 快照（启动时从 resolver catalog 取）。
    pub(in crate::tui) fn set_available_skills(&mut self, skills: Vec<SkillEntry>) {
        self.skills = skills;
    }

    /// 设置可用的 prompt templates（启动时从 resolver catalog 取）。
    pub(in crate::tui) fn set_available_templates(&mut self, templates: Vec<PromptTemplate>) {
        self.templates = templates;
    }

    /// 可用的 prompt templates（模板调用展开用）。
    pub(super) fn templates(&self) -> &[PromptTemplate] {
        &self.templates
    }

    // ── 草稿读取 ────────────────────────────────────────────────────────────

    pub(in crate::tui) fn text(&self) -> &str {
        &self.text
    }

    /// 光标位置（逻辑行号, 行内显示宽度）：多行输入框渲染光标用。
    pub(in crate::tui) fn cursor_position(&self) -> (u16, u16) {
        let before = &self.text[..self.cursor];
        let row = before.bytes().filter(|b| *b == b'\n').count();
        let col = before.rsplit('\n').next().map_or(0, UnicodeWidthStr::width);
        (
            u16::try_from(row).unwrap_or(u16::MAX),
            u16::try_from(col).unwrap_or(u16::MAX),
        )
    }

    /// 输入的逻辑行数（空输入为 1），输入框高度据此伸缩。
    pub(in crate::tui) fn line_count(&self) -> u16 {
        line_count_of(&self.text)
    }

    // ── 编辑 ────────────────────────────────────────────────────────────────

    pub(super) fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.refresh_completion();
    }

    /// Shift+Enter 手动换行：换行是空白字符，补全弹层随之关闭。
    pub(super) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// 粘贴一段文本到光标处（可含换行；`\r\n` 统一为 `\n`），随后重算补全。
    pub(super) fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.refresh_completion();
    }

    /// 编辑器写回（INSERT `Ctrl+G` 外部编辑器退出）：编辑器内容整体替换
    /// 输入缓冲（编辑器是权威副本），`\r\n` 归一为 `\n`、去掉文件尾空白，
    /// 光标移到末尾并重算补全；空白内容返回 `false`（保存空文件是常见
    /// 误操作，不应清掉已有输入，提示语由模式路由层落到 notice）。
    pub(super) fn apply_editor_result(&mut self, text: &str) -> bool {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.trim_end();
        if text.is_empty() {
            return false;
        }
        self.text = text.to_string();
        self.cursor = self.text.len();
        self.refresh_completion();
        true
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .char_indices()
            .last()
            .map_or(0, |(index, _)| index);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.refresh_completion();
    }

    pub(super) fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .last()
                .map_or(0, |(index, _)| index);
            self.refresh_completion();
        }
    }

    pub(super) fn cursor_right(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
            self.refresh_completion();
        }
    }

    pub(super) fn cursor_home(&mut self) {
        self.cursor = 0;
        self.refresh_completion();
    }

    pub(super) fn cursor_end(&mut self) {
        self.cursor = self.text.len();
        self.refresh_completion();
    }

    /// Ctrl+A：光标移到当前逻辑行开头（多行输入只作用当前行）。
    pub(super) fn cursor_line_home(&mut self) {
        let start = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        if start != self.cursor {
            self.cursor = start;
            self.refresh_completion();
        }
    }

    /// Ctrl+E：光标移到当前逻辑行末尾（多行输入只作用当前行）。
    pub(super) fn cursor_line_end(&mut self) {
        let end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |offset| self.cursor + offset);
        if end != self.cursor {
            self.cursor = end;
            self.refresh_completion();
        }
    }

    /// Ctrl+U：删除到当前逻辑行开头（多行输入只清当前行）。
    pub(super) fn delete_to_line_start(&mut self) {
        let start = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        if start < self.cursor {
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
            self.refresh_completion();
        }
    }

    /// Ctrl+W：删除光标前的一个词（连同词前的空白间隔）。
    pub(super) fn delete_word_back(&mut self) {
        let target = self.word_left_pos();
        if target < self.cursor {
            self.text.replace_range(target..self.cursor, "");
            self.cursor = target;
            self.refresh_completion();
        }
    }

    /// Alt+B：光标移到前一个词的开头。
    pub(super) fn cursor_word_left(&mut self) {
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
        for (index, c) in self.text[..self.cursor].char_indices().rev() {
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
    pub(super) fn cursor_word_right(&mut self) {
        let target = self.word_right_pos();
        if target != self.cursor {
            self.cursor = target;
            self.refresh_completion();
        }
    }

    /// 光标后一词开头的字节索引（Alt+F 用）。
    fn word_right_pos(&self) -> usize {
        let after = &self.text[self.cursor..];
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
        self.text.len() - rest.len() + gap_len
    }

    /// 取出待提交的输入并清空缓冲；空输入返回 `None`。
    pub(super) fn take_input(&mut self) -> Option<String> {
        let text = self.text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.text.clear();
        self.cursor = 0;
        self.completion = None;
        self.mention = None;
        Some(text)
    }

    /// 整体替换草稿（QUEUE 就地编辑载入槽位文本）：光标置于末尾，
    /// 补全弹层清空。
    pub(super) fn set_text(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.completion = None;
        self.mention = None;
    }

    /// 清空草稿（QUEUE 就地编辑保存后）。
    pub(super) fn clear_buffer(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.mention = None;
    }

    /// 清空草稿与附件（INSERT `Ctrl+C` 清草稿：文本与暂存图片一并丢弃）。
    pub(super) fn clear_draft(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.completion = None;
        self.mention = None;
        self.attachments.clear();
    }

    /// 删除光标处字符（INSERT `Ctrl+D` 的 readline 语义，光标不动）。
    pub(super) fn delete_char_at_cursor(&mut self) {
        if let Some(c) = self.text[self.cursor..].chars().next() {
            self.text
                .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
            self.refresh_completion();
        }
    }

    // ── 附件 ────────────────────────────────────────────────────────────────

    /// 暂存一张图片附件，返回当前附件总数。
    pub(in crate::tui) fn stage_image(
        &mut self,
        name: String,
        image: nomic_ai::ImageContent,
    ) -> usize {
        self.attachments.push(PendingImage { name, image });
        self.attachments.len()
    }

    /// 是否有暂存的图片附件。
    pub(in crate::tui) const fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    /// 取出全部暂存附件（prompt 提交时随文本一起带走）。
    pub(super) fn take_attachments(&mut self) -> Vec<nomic_ai::ImageContent> {
        self.attachments
            .drain(..)
            .map(|pending| pending.image)
            .collect()
    }

    /// 附件展示名列表（输入框附件行渲染用）。
    pub(in crate::tui) fn attachment_names(&self) -> impl Iterator<Item = &str> {
        self.attachments.iter().map(|pending| pending.name.as_str())
    }

    // ── 命令补全 ────────────────────────────────────────────────────────

    /// 当前补全弹层（渲染用）。
    pub(in crate::tui) const fn completion(&self) -> Option<&Completion> {
        self.completion.as_ref()
    }

    /// 同步补全启用状态（命令栏启用、聊天草稿不启用；构造时设置）。
    pub(super) fn set_completion_enabled(&mut self, enabled: bool) {
        self.completion_enabled = enabled;
        if !enabled {
            self.completion = None;
        }
    }

    /// 按当前输入重算补全候选：仅在「补全启用、光标在末尾、命令名未
    /// 输入完整参数（无空白）」时弹出（空片段列出全部命令与模板）；
    /// `skill:` 后切换为 skill 名候选。
    pub(super) fn refresh_completion(&mut self) {
        // mention 补全与命令补全独立：草稿（命令补全关闭）也要能弹 mention。
        self.refresh_mention();
        if !self.completion_enabled {
            self.completion = None;
            return;
        }
        let Some(fragment) = self.command_fragment().map(str::to_string) else {
            self.completion = None;
            return;
        };
        self.completion = if let Some(name_fragment) = fragment.strip_prefix("skill:") {
            self.skill_candidates(name_fragment)
        } else {
            self.command_candidates(&fragment)
        };
    }

    /// 内建命令与 prompt template 候选（按名称/别名前缀匹配，按名称排序；
    /// 同名时内建命令在前）。
    fn command_candidates(&self, fragment: &str) -> Option<Completion> {
        let mut candidates: Vec<CompletionCandidate> = COMMANDS
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

    /// `skill:` 后的 skill 名候选（按名称前缀匹配）。
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

    /// 光标位于末尾且输入是「无参数的命令名片段」时返回该片段
    ///（空片段列出全部命令与模板）。
    fn command_fragment(&self) -> Option<&str> {
        if self.cursor != self.text.len() || self.text.contains(char::is_whitespace) {
            return None;
        }
        Some(&self.text)
    }

    /// Tab：接受当前选中候选；输入已等于选中项时循环到下一个。
    pub(super) fn tab_complete(&mut self) {
        let Some(completion) = &self.completion else {
            return;
        };
        let current = completion.candidates[completion.selected].fragment();
        let selected = if self.text == current {
            (completion.selected + 1) % completion.candidates.len()
        } else {
            completion.selected
        };
        let fragment = completion.candidates[selected].fragment();
        self.text = fragment;
        self.cursor = self.text.len();
        self.refresh_completion();
    }

    /// 补全弹层中选择下一个/上一个候选（环形）。
    pub(super) const fn completion_select(&mut self, delta: isize) {
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
    pub(super) fn dismiss_completion(&mut self) -> bool {
        self.completion.take().is_some()
    }

    /// Enter 且补全弹层可见时的智能接受：输入未精确匹配任何候选时
    /// 填入选中候选（返回 `true`，不提交）；已精确匹配则返回 `false` 正常提交。
    pub(super) fn accept_completion_on_enter(&mut self) -> bool {
        let Some(fragment) = self.command_fragment() else {
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

    // ── `@` mention 补全 ────────────────────────────────────────────────────

    /// 当前 mention 补全弹层（渲染用）。
    pub(in crate::tui) const fn mention(&self) -> Option<&MentionCompletion> {
        self.mention.as_ref()
    }

    /// 同步 mention 补全启用状态（草稿启用、命令栏不启用）。
    pub(super) fn set_mention_enabled(&mut self, enabled: bool) {
        self.mention_enabled = enabled;
        if !enabled {
            self.mention = None;
        }
    }

    /// 按当前输入重算 mention 候选：仅当「mention 启用、光标在末尾、文本以
    /// `@` 收尾且 `@` 后无空白」时弹出。
    fn refresh_mention(&mut self) {
        if !self.mention_enabled || self.cursor != self.text.len() {
            self.mention = None;
            return;
        }
        let Some(fragment) = mention::mention_fragment(&self.text) else {
            self.mention = None;
            return;
        };
        self.mention = self.mention_candidates(fragment);
    }

    /// 按 mention 片段构造候选：`@skill:` 后为 skill 名、`@file:` 后为文件
    /// 路径；`@` + 部分 `skill`/`file` 前缀时为类型候选。
    fn mention_candidates(&self, fragment: &str) -> Option<MentionCompletion> {
        if let Some(name) = fragment.strip_prefix(mention::SKILL_PREFIX) {
            return self.skill_mention_candidates(name);
        }
        if let Some(path) = fragment.strip_prefix(mention::FILE_PREFIX) {
            return Self::file_mention_candidates(path);
        }
        let prefix = fragment.strip_prefix('@')?;
        let mut candidates = Vec::new();
        if "skill".starts_with(prefix) {
            candidates.push(MentionCandidate {
                fragment: mention::SKILL_PREFIX.to_string(),
                display: mention::SKILL_PREFIX.to_string(),
            });
        }
        if "file".starts_with(prefix) {
            candidates.push(MentionCandidate {
                fragment: mention::FILE_PREFIX.to_string(),
                display: mention::FILE_PREFIX.to_string(),
            });
        }
        if candidates.is_empty() {
            return None;
        }
        Some(MentionCompletion {
            candidates,
            selected: 0,
        })
    }

    /// `@skill:` 后的 skill 名候选（按名称前缀匹配）。
    fn skill_mention_candidates(&self, name_fragment: &str) -> Option<MentionCompletion> {
        let mut candidates: Vec<MentionCandidate> = self
            .skills
            .iter()
            .filter(|entry| entry.name.starts_with(name_fragment))
            .map(|entry| MentionCandidate {
                fragment: format!("@skill:{}", entry.name),
                display: format!("@skill:{} — {}", entry.name, entry.description),
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by(|a, b| a.fragment.cmp(&b.fragment));
        Some(MentionCompletion {
            candidates,
            selected: 0,
        })
    }

    /// `@file:` 后的文件路径候选（相对当前工作目录，按前缀匹配）。
    fn file_mention_candidates(path_fragment: &str) -> Option<MentionCompletion> {
        let cwd = std::env::current_dir().unwrap_or_default();
        let candidates: Vec<MentionCandidate> =
            mention::file_mention_candidates(path_fragment, &cwd)
                .into_iter()
                .map(|path| MentionCandidate {
                    fragment: format!("@file:{path}"),
                    display: format!("@file:{path}"),
                })
                .collect();
        if candidates.is_empty() {
            return None;
        }
        Some(MentionCompletion {
            candidates,
            selected: 0,
        })
    }

    /// Tab：接受当前选中的 mention 候选，替换掉光标处的 mention 片段；
    /// 片段已等于选中项时循环到下一个。
    pub(super) fn mention_tab_complete(&mut self) {
        let Some(at) = self.text.rfind('@') else {
            return;
        };
        let current = self.text[at..].to_string();
        let Some(completion) = &self.mention else {
            return;
        };
        let selected = if current == completion.candidates[completion.selected].fragment {
            (completion.selected + 1) % completion.candidates.len()
        } else {
            completion.selected
        };
        let fragment = completion.candidates[selected].fragment.clone();
        self.text.truncate(at);
        self.text.push_str(&fragment);
        self.cursor = self.text.len();
        self.refresh_completion();
    }

    /// mention 补全弹层中选择下一个/上一个候选（环形）。
    pub(super) const fn mention_select(&mut self, delta: isize) {
        if let Some(mention) = &mut self.mention {
            let len = mention.candidates.len();
            let step = delta.unsigned_abs() % len;
            mention.selected = if delta < 0 {
                (mention.selected + len - step) % len
            } else {
                (mention.selected + step) % len
            };
        }
    }

    /// Esc：关闭 mention 补全弹层；返回是否确有弹层被关闭。
    pub(super) fn dismiss_mention(&mut self) -> bool {
        self.mention.take().is_some()
    }
}

/// 词字符判定（INSERT 词级移动/删除共用）：字母数字与下划线。
/// CJK 字符的 `is_alphanumeric` 为真，连续中文视为一个长词。
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `skill` 无参时展示的可用 skill 清单（本地展示，不进上下文）。
pub(super) fn skill_list_text(skills: &[SkillEntry]) -> String {
    use std::fmt::Write as _;
    if skills.is_empty() {
        return "没有可用的 skill（查找 .nomic/skills、.agents/skills 与用户配置目录）。"
            .to_string();
    }
    let mut text = "可用 skill（skill:<name> 载入）：".to_string();
    for skill in skills {
        let _ = write!(
            text,
            "\n  {} — {}（{}）",
            skill.name, skill.description, skill.scope
        );
    }
    text
}
