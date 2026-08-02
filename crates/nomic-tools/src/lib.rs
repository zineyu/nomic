//! nomic-tools：编码 agent 的四个基础工具（对应 pi-coding-agent 的工具层）。
//!
//! 工具的输出格式与引导提示（截断翻页、diff 详情、错误文本）是与模型的
//! 契约，忠实复刻 pi 的措辞以保证模型行为质量。

mod bash;
mod edit;
mod mutation_queue;
mod read;
mod truncate;
mod write;

pub use bash::BashTool;
pub use edit::EditTool;
pub use read::ReadTool;
pub use truncate::{
    Continuation, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, Truncation, exceeds_notice,
    truncate_head, truncate_tail,
};
pub use write::WriteTool;

/// 创建默认四件套的 [`nomic_core::DynTool`] 列表。
pub fn default_tools() -> Vec<nomic_core::DynTool> {
    vec![
        nomic_core::DynTool::new(ReadTool::new()),
        nomic_core::DynTool::new(WriteTool),
        nomic_core::DynTool::new(EditTool),
        nomic_core::DynTool::new(BashTool),
    ]
}

/// 创建支持 `skill://` 的默认四件套。
pub fn default_tools_with_skills(
    skill_resolver: nomic_skills::SkillResolver,
) -> Vec<nomic_core::DynTool> {
    vec![
        nomic_core::DynTool::new(ReadTool::with_skill_resolver(skill_resolver)),
        nomic_core::DynTool::new(WriteTool),
        nomic_core::DynTool::new(EditTool),
        nomic_core::DynTool::new(BashTool),
    ]
}
