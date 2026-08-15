//! nomic-tools：编码 agent 的基础工具（对应 pi-coding-agent 的工具层）。
//!
//! 八个工具：read/write/edit/bash 负责文件读写与命令执行，
//! grep/find 提供 ripgrep/fd 语义的内容搜索与文件查找，
//! todo_read/todo_write 提供带父子层级的任务清单（共享内存态 [`TodoStore`]），
//! ask_user_question 经 [`QuestionSink`] 向用户提问（单选/多选/填空）。
//!
//! 工具的输出格式与引导提示（截断翻页、diff 详情、错误文本）是与模型的
//! 契约，忠实复刻 pi 的措辞以保证模型行为质量。

mod ask;
mod bash;
mod edit;
mod find;
mod grep;
mod mutation_queue;
mod read;
mod todo;
mod truncate;
mod walk;
mod write;

pub use ask::{
    AskUserAnswer, AskUserQuestion, AskUserQuestionParams, AskUserQuestionTool, CUSTOM_OPTION,
    QuestionKind, QuestionSink,
};
pub use bash::BashTool;
pub use edit::EditTool;
pub use find::FindTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use todo::{
    TodoItem, TodoItemInput, TodoReadTool, TodoStatus, TodoStore, TodoWriteParams, TodoWriteTool,
    render_todos,
};
pub use truncate::{
    Continuation, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, Truncation, exceeds_notice,
    truncate_head, truncate_tail,
};
pub use write::WriteTool;

/// 创建默认工具集的 [`nomic_core::DynTool`] 列表。
///
/// todo 工具共享调用方持有的 [`TodoStore`]（clone 即共享同一份数据），
/// 交互端可据此在 run 结束后检查未完成的 todo（goal 模式）；
/// ask_user_question 经调用方提供的 [`QuestionSink`] 与用户交互。
pub fn default_tools(
    todo_store: TodoStore,
    question_sink: std::sync::Arc<dyn QuestionSink>,
) -> Vec<nomic_core::DynTool> {
    vec![
        nomic_core::DynTool::new(ReadTool::new()),
        nomic_core::DynTool::new(WriteTool),
        nomic_core::DynTool::new(EditTool),
        nomic_core::DynTool::new(BashTool),
        nomic_core::DynTool::new(GrepTool),
        nomic_core::DynTool::new(FindTool),
        nomic_core::DynTool::new(TodoReadTool::new(todo_store.clone())),
        nomic_core::DynTool::new(TodoWriteTool::new(todo_store)),
        nomic_core::DynTool::new(AskUserQuestionTool::new(question_sink)),
    ]
}

/// 创建支持 `skill://` 的默认工具集（todo store 语义同 [`default_tools`]）。
pub fn default_tools_with_skills(
    skill_resolver: nomic_skills::SkillResolver,
    todo_store: TodoStore,
    question_sink: std::sync::Arc<dyn QuestionSink>,
) -> Vec<nomic_core::DynTool> {
    vec![
        nomic_core::DynTool::new(ReadTool::with_skill_resolver(skill_resolver)),
        nomic_core::DynTool::new(WriteTool),
        nomic_core::DynTool::new(EditTool),
        nomic_core::DynTool::new(BashTool),
        nomic_core::DynTool::new(GrepTool),
        nomic_core::DynTool::new(FindTool),
        nomic_core::DynTool::new(TodoReadTool::new(todo_store.clone())),
        nomic_core::DynTool::new(TodoWriteTool::new(todo_store)),
        nomic_core::DynTool::new(AskUserQuestionTool::new(question_sink)),
    ]
}
