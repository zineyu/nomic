//! `todo_read` / `todo_write` 工具：带父子层级的任务清单。
//!
//! 供模型把复杂任务拆解为 todo 树（父 todo 可嵌套子 todo）、追踪进度，
//! 并把当前状态渲染回上下文。两个工具共享一个 [`TodoStore`]（每个 agent
//! 实例一份，内存态，不落盘）。
//!
//! 采用全量替换语义：`todo_write` 每次提交完整清单，与 Claude Code 的
//! TodoWrite 契约一致——模型无需维护增量 diff，单个 `execute` 调用天然原子。

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use nomic_core::{AgentTool, ToolError, ToolResult, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Todo 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// 未开始
    Pending,
    /// 进行中
    InProgress,
    /// 已完成
    Completed,
    /// 已取消（不再需要）
    Cancelled,
}

impl TodoStatus {
    const fn marker(self) -> &'static str {
        match self {
            Self::Pending => "[ ]",
            Self::InProgress => "[~]",
            Self::Completed => "[x]",
            Self::Cancelled => "[-]",
        }
    }

    const fn count_label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in progress",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 一条 todo（可嵌套子 todo）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TodoItem {
    /// Unique id of the todo (unique across the whole tree)
    pub id: String,
    /// Short title describing the task
    pub title: String,
    /// Current status
    pub status: TodoStatus,
    /// Sub-todos of this todo
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TodoItem>,
}

/// `todo_write` 的输入项；`id` 可省略（自动分配 `t1`、`t2`…）。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TodoItemInput {
    /// Unique id of the todo; omit to auto-assign one
    #[serde(default)]
    pub id: Option<String>,
    /// Short title describing the task
    pub title: String,
    /// Current status
    pub status: TodoStatus,
    /// Sub-todos of this todo
    #[serde(default)]
    pub children: Vec<TodoItemInput>,
}

/// 共享的 todo 清单（线程安全，clone 即共享同一份数据）。
#[derive(Debug, Clone, Default)]
pub struct TodoStore {
    todos: Arc<Mutex<Vec<TodoItem>>>,
}

impl TodoStore {
    /// 创建空清单。
    pub fn new() -> Self {
        Self::default()
    }

    /// 读取当前清单（深拷贝）。
    ///
    /// # Panics
    /// 锁中毒时 panic（持有方 panic 属于不可恢复的内部错误）。
    pub fn todos(&self) -> Vec<TodoItem> {
        self.lock().clone()
    }

    /// 全量替换清单。
    fn replace(&self, todos: Vec<TodoItem>) {
        *self.lock() = todos;
    }

    fn lock(&self) -> MutexGuard<'_, Vec<TodoItem>> {
        self.todos.lock().expect("todo store lock poisoned")
    }
}

/// 把输入项校验并落实为存储项：检查空标题、重复 id，为缺失的 id 自动分配。
fn materialize(inputs: &[TodoItemInput]) -> Result<Vec<TodoItem>, ToolError> {
    let mut used = HashSet::new();
    collect_ids(inputs, &mut used)?;
    let mut next = 1u32;
    materialize_inner(inputs, &mut used, &mut next)
}

fn collect_ids<'a>(
    inputs: &'a [TodoItemInput],
    used: &mut HashSet<&'a str>,
) -> Result<(), ToolError> {
    for input in inputs {
        if input.title.trim().is_empty() {
            return Err(ToolError::new("todo title must not be empty"));
        }
        if let Some(id) = &input.id {
            if id.trim().is_empty() {
                return Err(ToolError::new("todo id must not be empty"));
            }
            if !used.insert(id.as_str()) {
                return Err(ToolError::new(format!("duplicate todo id: {id}")));
            }
        }
        collect_ids(&input.children, used)?;
    }
    Ok(())
}

fn materialize_inner(
    inputs: &[TodoItemInput],
    used: &mut HashSet<&str>,
    next: &mut u32,
) -> Result<Vec<TodoItem>, ToolError> {
    inputs
        .iter()
        .map(|input| {
            let id = if let Some(id) = &input.id {
                id.clone()
            } else {
                // 跳过已被占用的序号，保证整棵树内唯一
                while used.contains(format!("t{next}").as_str()) {
                    *next += 1;
                }
                let id = format!("t{next}");
                *next += 1;
                id
            };
            Ok(TodoItem {
                id,
                title: input.title.trim().to_string(),
                status: input.status,
                children: materialize_inner(&input.children, used, next)?,
            })
        })
        .collect()
}

#[derive(Default)]
struct Counts {
    pending: usize,
    in_progress: usize,
    completed: usize,
    cancelled: usize,
}

impl Counts {
    const fn add(&mut self, status: TodoStatus) {
        match status {
            TodoStatus::Pending => self.pending += 1,
            TodoStatus::InProgress => self.in_progress += 1,
            TodoStatus::Completed => self.completed += 1,
            TodoStatus::Cancelled => self.cancelled += 1,
        }
    }

    const fn total(&self) -> usize {
        self.pending + self.in_progress + self.completed + self.cancelled
    }

    fn parts(&self) -> String {
        [
            (TodoStatus::Pending, self.pending),
            (TodoStatus::InProgress, self.in_progress),
            (TodoStatus::Completed, self.completed),
            (TodoStatus::Cancelled, self.cancelled),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(status, n)| format!("{n} {}", status.count_label()))
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// 渲染为带缩进的树形文本（回喂模型的格式）。
fn render(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "No todos.".to_string();
    }
    let mut counts = Counts::default();
    let mut body = String::new();
    render_items(todos, 0, &mut counts, &mut body);
    format!("{} todo(s) ({}):\n{body}", counts.total(), counts.parts())
}

fn render_items(todos: &[TodoItem], depth: usize, counts: &mut Counts, out: &mut String) {
    for todo in todos {
        counts.add(todo.status);
        let indent = "    ".repeat(depth);
        let _ = writeln!(
            out,
            "{indent}{} {} · {}",
            todo.status.marker(),
            todo.id,
            todo.title
        );
        render_items(&todo.children, depth + 1, counts, out);
    }
}

/// `todo_read` 工具：读取当前 todo 清单。
#[derive(Debug, Clone)]
pub struct TodoReadTool {
    store: TodoStore,
}

/// `todo_read` 参数（无）。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TodoReadParams {}

const READ_LABEL: &str = "todo_read";
const READ_DESCRIPTION: &str =
    "Read the current todo list. Use this to check task progress before planning next steps.";

impl TodoReadTool {
    /// 绑定共享清单。
    pub const fn new(store: TodoStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for TodoReadTool {
    type Params = TodoReadParams;

    fn name(&self) -> &'static str {
        "todo_read"
    }

    fn label(&self) -> &str {
        READ_LABEL
    }

    fn description(&self) -> &str {
        READ_DESCRIPTION
    }

    async fn execute(
        &self,
        _params: Self::Params,
        _cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        let todos = self.store.todos();
        Ok(ToolResult {
            details: Some(serde_json::json!({ "todos": todos })),
            ..ToolResult::text(render(&todos))
        })
    }
}

/// `todo_write` 工具：全量替换 todo 清单（支持父子嵌套）。
#[derive(Debug, Clone)]
pub struct TodoWriteTool {
    store: TodoStore,
}

/// `todo_write` 参数。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TodoWriteParams {
    /// The complete todo list, replacing the current one. Use `children` to nest sub-todos under a parent todo.
    pub todos: Vec<TodoItemInput>,
}

const WRITE_LABEL: &str = "todo_write";
const WRITE_DESCRIPTION: &str = "Replace the todo list with a new one. Use this to break down complex tasks \
         into steps (nest sub-todos via `children`), track progress, and mark items \
         in_progress before starting and completed right after finishing. The list \
         is replaced as a whole: include every item you want to keep.";

impl TodoWriteTool {
    /// 绑定共享清单。
    pub const fn new(store: TodoStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl AgentTool for TodoWriteTool {
    type Params = TodoWriteParams;

    fn name(&self) -> &'static str {
        "todo_write"
    }

    fn label(&self) -> &str {
        WRITE_LABEL
    }

    fn description(&self) -> &str {
        WRITE_DESCRIPTION
    }

    async fn execute(
        &self,
        params: Self::Params,
        cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::new("Operation aborted"));
        }
        let todos = materialize(&params.todos)?;
        self.store.replace(todos.clone());
        Ok(ToolResult {
            details: Some(serde_json::json!({ "todos": todos })),
            ..ToolResult::text(format!("Todos updated.\n{}", render(&todos)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_update() -> ToolUpdateCallback {
        Box::new(|_| {})
    }

    fn item(title: &str, status: TodoStatus, children: Vec<TodoItemInput>) -> TodoItemInput {
        TodoItemInput {
            id: None,
            title: title.to_string(),
            status,
            children,
        }
    }

    async fn write(
        tool: &TodoWriteTool,
        todos: Vec<TodoItemInput>,
    ) -> Result<ToolResult, ToolError> {
        tool.execute(
            TodoWriteParams { todos },
            CancellationToken::new(),
            noop_update(),
        )
        .await
    }

    async fn read_text(tool: &TodoReadTool) -> String {
        let result = tool
            .execute(TodoReadParams {}, CancellationToken::new(), noop_update())
            .await
            .expect("todo_read 不应失败");
        let [nomic_ai::UserContent::Text(text)] = &result.content[..] else {
            panic!("expected text result");
        };
        text.text.clone()
    }

    fn result_text(result: &ToolResult) -> &str {
        let [nomic_ai::UserContent::Text(text)] = &result.content[..] else {
            panic!("expected text result");
        };
        &text.text
    }

    #[tokio::test]
    async fn empty_store_reads_no_todos() {
        let store = TodoStore::new();
        let read = TodoReadTool::new(store);
        assert_eq!(read_text(&read).await, "No todos.");
    }

    #[tokio::test]
    async fn write_assigns_ids_and_read_renders_tree() {
        let store = TodoStore::new();
        let write_tool = TodoWriteTool::new(store.clone());
        let read_tool = TodoReadTool::new(store);

        let result = write(
            &write_tool,
            vec![
                item(
                    "实现 todo 工具",
                    TodoStatus::InProgress,
                    vec![
                        item("编写存储", TodoStatus::Completed, vec![]),
                        item("编写测试", TodoStatus::Pending, vec![]),
                    ],
                ),
                item("更新文档", TodoStatus::Pending, vec![]),
            ],
        )
        .await
        .expect("写入应成功");

        let text = result_text(&result);
        assert!(
            text.contains("4 todo(s) (2 pending, 1 in progress, 1 completed)"),
            "{text}"
        );
        // 父 todo 与子 todo 的缩进层级
        let read_back = read_text(&read_tool).await;
        assert!(
            read_back.contains("[~] t1 · 实现 todo 工具\n    [x] t2 · 编写存储\n    [ ] t3 · 编写测试\n[ ] t4 · 更新文档"),
            "{read_back}"
        );
        // details 携带结构化清单（UI 渲染用）
        let details = result.details.expect("应携带 details");
        assert_eq!(details["todos"][0]["children"][0]["id"], "t2");
    }

    #[tokio::test]
    async fn write_replaces_previous_list() {
        let store = TodoStore::new();
        let write_tool = TodoWriteTool::new(store.clone());
        write(
            &write_tool,
            vec![item("旧任务", TodoStatus::Pending, vec![])],
        )
        .await
        .expect("首次写入应成功");
        write(
            &write_tool,
            vec![item("新任务", TodoStatus::Pending, vec![])],
        )
        .await
        .expect("第二次写入应成功");
        let text = read_text(&TodoReadTool::new(store)).await;
        assert!(!text.contains("旧任务"), "{text}");
        assert!(text.contains("新任务"), "{text}");
    }

    #[tokio::test]
    async fn duplicate_ids_rejected() {
        let store = TodoStore::new();
        let write_tool = TodoWriteTool::new(store);
        let mut a = item("任务 a", TodoStatus::Pending, vec![]);
        a.id = Some("x".to_string());
        let mut b = item("任务 b", TodoStatus::Pending, vec![]);
        b.id = Some("x".to_string());
        let error = write(&write_tool, vec![a, b]).await.unwrap_err();
        assert!(
            error.to_string().contains("duplicate todo id: x"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn empty_title_rejected() {
        let store = TodoStore::new();
        let write_tool = TodoWriteTool::new(store);
        let error = write(&write_tool, vec![item("   ", TodoStatus::Pending, vec![])])
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("title must not be empty"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn auto_ids_skip_user_provided_ids() {
        let store = TodoStore::new();
        let write_tool = TodoWriteTool::new(store);
        let mut explicit = item("显式 id", TodoStatus::Pending, vec![]);
        explicit.id = Some("t1".to_string());
        let result = write(
            &write_tool,
            vec![explicit, item("自动 id", TodoStatus::Pending, vec![])],
        )
        .await
        .expect("写入应成功");
        let text = result_text(&result);
        assert!(text.contains("[ ] t1 · 显式 id"), "{text}");
        assert!(text.contains("[ ] t2 · 自动 id"), "{text}");
    }
}
