//! nomic-tools：编码 agent 的基础工具（对应 pi-coding-agent 的工具层）。
//!
//! 八个工具：read/write/edit/bash 负责文件读写与命令执行，
//! grep/find 提供 ripgrep/fd 语义的内容搜索与文件查找，
//! todo_read/todo_write 提供带父子层级的任务清单（共享内存态 [`TodoStore`]）。
//!
//! 工具的输出格式与引导提示（截断翻页、diff 详情、错误文本）是与模型的
//! 契约，忠实复刻 pi 的措辞以保证模型行为质量。

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

pub use bash::BashTool;
pub use edit::EditTool;
pub use find::FindTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use todo::{TodoItem, TodoReadTool, TodoStatus, TodoStore, TodoWriteTool};
pub use truncate::{
    Continuation, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, Truncation, exceeds_notice,
    truncate_head, truncate_tail,
};
pub use write::WriteTool;

/// 创建默认工具集的 [`nomic_core::DynTool`] 列表。
pub fn default_tools() -> Vec<nomic_core::DynTool> {
    let todo_store = TodoStore::new();
    vec![
        nomic_core::DynTool::new(ReadTool::new()),
        nomic_core::DynTool::new(WriteTool),
        nomic_core::DynTool::new(EditTool),
        nomic_core::DynTool::new(BashTool),
        nomic_core::DynTool::new(GrepTool),
        nomic_core::DynTool::new(FindTool),
        nomic_core::DynTool::new(TodoReadTool::new(todo_store.clone())),
        nomic_core::DynTool::new(TodoWriteTool::new(todo_store)),
    ]
}

/// 创建支持 `skill://` 的默认工具集。
pub fn default_tools_with_skills(
    skill_resolver: nomic_skills::SkillResolver,
) -> Vec<nomic_core::DynTool> {
    let todo_store = TodoStore::new();
    vec![
        nomic_core::DynTool::new(ReadTool::with_skill_resolver(skill_resolver)),
        nomic_core::DynTool::new(WriteTool),
        nomic_core::DynTool::new(EditTool),
        nomic_core::DynTool::new(BashTool),
        nomic_core::DynTool::new(GrepTool),
        nomic_core::DynTool::new(FindTool),
        nomic_core::DynTool::new(TodoReadTool::new(todo_store.clone())),
        nomic_core::DynTool::new(TodoWriteTool::new(todo_store)),
    ]
}
