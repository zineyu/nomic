//! nomic-tools：编码 agent 的基础工具（对应 pi-coding-agent 的工具层）。
//!
//! 八个工具：read/write/edit/bash 负责文件读写与命令执行，
//! grep/find 提供内容搜索与文件查找（基于 fff 常驻索引：后台扫描 +
//! watcher 保持新鲜，重复搜索近即时），
//! todo_read/todo_write 提供带父子层级的任务清单（共享内存态 [`TodoStore`]），
//! ask_user_question 经 [`QuestionSink`] 向用户提问（单选/多选/填空），
//! goal_done 是 goal 模式的完成汇报通道（经共享 [`GoalSession`] 通知交互端，
//! 见 [`goal_tools`] 的工具集变换），「run 停止时复述目标」的追问策略收在
//! [`GoalNudger`]（TUI / web 共用），
//! 宿主侧的在途提问生命周期（登记/应答/丢弃/快照）收在 [`QuestionRegistry`]。
//!
//! 工具的输出格式与引导提示（截断翻页、diff 详情、错误文本）是与模型的
//! 契约，忠实复刻 pi 的措辞以保证模型行为质量。

mod ask;
mod base;
mod bash;
pub mod conflicts;
mod edit;
mod find;
mod goal;
mod grep;
pub mod multi_agent;
mod mutation_queue;
pub mod nix_env;
mod picker;
mod question_registry;
mod read;
mod todo;
mod truncate;
mod vfs_guard;
mod write;

pub use ask::{
    AskUserAnswer, AskUserQuestion, AskUserQuestionParams, AskUserQuestionTool, CUSTOM_OPTION,
    QuestionKind, QuestionSink,
};
pub use base::BaseDir;
pub use bash::{BashParams, BashTool};
pub use edit::EditTool;
pub use find::FindTool;
pub use goal::{
    GoalDoneParams, GoalDoneTool, GoalNudger, GoalSession, Nudge, goal_prompt, goal_tools,
};
pub use grep::GrepTool;
pub use question_registry::QuestionRegistry;
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
/// 交互端可持有句柄观察 agent 写入的任务清单；
/// ask_user_question 经调用方提供的 [`QuestionSink`] 与用户交互。
/// 相对路径以进程 cwd 为基准；workspace 归属场景用 [`default_tools_in`]。
pub fn default_tools(
    todo_store: TodoStore,
    question_sink: std::sync::Arc<dyn QuestionSink>,
) -> Vec<nomic_core::DynTool> {
    default_tools_in(None, todo_store, question_sink)
}

/// 以 `base_dir` 为相对路径基准的默认工具集（`None` = 进程 cwd）。
pub fn default_tools_in(
    base_dir: Option<std::path::PathBuf>,
    todo_store: TodoStore,
    question_sink: std::sync::Arc<dyn QuestionSink>,
) -> Vec<nomic_core::DynTool> {
    default_tools_in_shared(&BaseDir::new(base_dir), todo_store, question_sink)
}

/// 以共享基准目录句柄构建默认工具集：句柄更新（[`BaseDir::set`]）后
/// 各工具的下一次执行即用新基准（交互端切换 session workspace 场景）。
pub fn default_tools_in_shared(
    base: &BaseDir,
    todo_store: TodoStore,
    question_sink: std::sync::Arc<dyn QuestionSink>,
) -> Vec<nomic_core::DynTool> {
    let nix_env = nix_env::NixEnvCache::new();
    prewarm_nix_env(&nix_env, base);
    vec![
        nomic_core::DynTool::new(ReadTool::new().with_shared_base_dir(base)),
        nomic_core::DynTool::new(WriteTool::new().with_shared_base_dir(base)),
        nomic_core::DynTool::new(EditTool::new().with_shared_base_dir(base)),
        nomic_core::DynTool::new(
            BashTool::new()
                .with_nix_env(nix_env)
                .with_shared_base_dir(base),
        ),
        nomic_core::DynTool::new(GrepTool::new().with_shared_base_dir(base)),
        nomic_core::DynTool::new(FindTool::new().with_shared_base_dir(base)),
        nomic_core::DynTool::new(TodoReadTool::new(todo_store.clone())),
        nomic_core::DynTool::new(TodoWriteTool::new(todo_store)),
        nomic_core::DynTool::new(AskUserQuestionTool::new(question_sink)),
    ]
}

/// 创建支持 `skill://` 的默认工具集（todo store 语义同 [`default_tools`]）。
/// 相对路径以进程 cwd 为基准；workspace 归属场景用
/// [`default_tools_with_skills_in`]。
pub fn default_tools_with_skills(
    skill_resolver: nomic_skills::SkillResolver,
    todo_store: TodoStore,
    question_sink: std::sync::Arc<dyn QuestionSink>,
) -> Vec<nomic_core::DynTool> {
    default_tools_with_skills_in(None, skill_resolver, todo_store, question_sink)
}

/// 以 `base_dir` 为相对路径基准、支持 `skill://` 的默认工具集。
pub fn default_tools_with_skills_in(
    base_dir: Option<std::path::PathBuf>,
    skill_resolver: nomic_skills::SkillResolver,
    todo_store: TodoStore,
    question_sink: std::sync::Arc<dyn QuestionSink>,
) -> Vec<nomic_core::DynTool> {
    default_tools_with_skills_in_shared(
        &BaseDir::new(base_dir),
        skill_resolver,
        todo_store,
        question_sink,
    )
}

/// 以共享基准目录句柄构建、支持 `skill://` 的默认工具集：句柄更新
///（[`BaseDir::set`]）后各工具的下一次执行即用新基准（交互端切换
/// session workspace 场景）。
pub fn default_tools_with_skills_in_shared(
    base: &BaseDir,
    skill_resolver: nomic_skills::SkillResolver,
    todo_store: TodoStore,
    question_sink: std::sync::Arc<dyn QuestionSink>,
) -> Vec<nomic_core::DynTool> {
    // 会话级 VFS 挂载表（ADR-0042）：工具共享同一实例，VFS 后端
    // 在构造期注入。后续协议（artifact/history…）在此追加挂载。
    let mut vfs_router = nomic_vfs::VfsRouter::new();
    vfs_router.mount(std::sync::Arc::new(nomic_vfs::fs::SkillVfs::new(
        skill_resolver,
    )));
    vfs_router.mount(std::sync::Arc::new(nomic_vfs::fs::LocalVfs::new(
        base.clone(),
    )));
    // nix://shell：workspace nix 环境定义（ADR-0041），与下方 BashTool 的
    // env 缓存经文件 mtime 解耦（改写即失效重解析，无需跨组件通知）
    vfs_router.mount(std::sync::Arc::new(nomic_vfs::fs::NixVfs::new(
        base.clone(),
    )));
    let vfs_router = std::sync::Arc::new(vfs_router);
    let nix_env = nix_env::NixEnvCache::new();
    prewarm_nix_env(&nix_env, base);
    vec![
        nomic_core::DynTool::new(
            ReadTool::with_vfs_router(vfs_router.clone()).with_shared_base_dir(base),
        ),
        nomic_core::DynTool::new(
            WriteTool::new()
                .with_vfs_router(vfs_router.clone())
                .with_shared_base_dir(base),
        ),
        nomic_core::DynTool::new(
            EditTool::new()
                .with_vfs_router(vfs_router.clone())
                .with_shared_base_dir(base),
        ),
        nomic_core::DynTool::new(
            BashTool::new()
                .with_vfs_router(vfs_router.clone())
                .with_nix_env(nix_env)
                .with_shared_base_dir(base),
        ),
        nomic_core::DynTool::new(
            GrepTool::new()
                .with_vfs_router(vfs_router)
                .with_shared_base_dir(base),
        ),
        nomic_core::DynTool::new(FindTool::new().with_shared_base_dir(base)),
        nomic_core::DynTool::new(TodoReadTool::new(todo_store.clone())),
        nomic_core::DynTool::new(TodoWriteTool::new(todo_store)),
        nomic_core::DynTool::new(AskUserQuestionTool::new(question_sink)),
    ]
}

/// 后台预解析 workspace 的 nix 环境（有 flake 时）：给首个 bash 调用
/// 提前量；无 flake / 无 tokio 运行时时为 no-op。
fn prewarm_nix_env(nix_env: &std::sync::Arc<nix_env::NixEnvCache>, base: &BaseDir) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    if let Some(dir) = base.snapshot() {
        nix_env.prewarm(&dir);
    }
}
