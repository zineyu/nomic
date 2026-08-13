use std::any::Any;

use super::{goal_reminder_prompt, panic_payload_text};
use nomic_core::AgentTool;
use nomic_tools::{TodoItemInput, TodoStatus, TodoStore, TodoWriteTool};

/// goal 模式追问提示词：列出未完成 todo；空清单或全部完成时不追问。
#[tokio::test]
async fn goal_reminder_lists_incomplete_todos() {
    async fn write(store: &TodoStore, todos: Vec<TodoItemInput>) {
        let tool = TodoWriteTool::new(store.clone());
        tool.execute(
            nomic_tools::TodoWriteParams { todos },
            tokio_util::sync::CancellationToken::new(),
            Box::new(|_| {}),
        )
        .await
        .expect("写入应成功");
    }
    let item = |title: &str, status: TodoStatus| TodoItemInput {
        id: None,
        title: title.to_string(),
        status,
        children: Vec::new(),
    };

    // 空清单：不追问
    let store = TodoStore::new();
    assert_eq!(goal_reminder_prompt(&store), None);

    // 有未完成项：提示词列出 pending / in_progress，不含 completed
    write(
        &store,
        vec![
            item("修复测试", TodoStatus::Completed),
            item("更新文档", TodoStatus::Pending),
            item("补充单测", TodoStatus::InProgress),
        ],
    )
    .await;
    let prompt = goal_reminder_prompt(&store).expect("有未完成项应追问");
    assert!(prompt.contains("[goal 模式]"), "{prompt}");
    assert!(prompt.contains("更新文档"), "{prompt}");
    assert!(prompt.contains("补充单测"), "{prompt}");
    assert!(!prompt.contains("修复测试"), "{prompt}");

    // 全部完成（含已取消）：不追问
    write(
        &store,
        vec![
            item("修复测试", TodoStatus::Completed),
            item("过时任务", TodoStatus::Cancelled),
        ],
    )
    .await;
    assert_eq!(goal_reminder_prompt(&store), None);
}

#[test]
fn panic_payload_extracts_message() {
    let payload: Box<dyn Any + Send> = Box::new("boom");
    assert_eq!(panic_payload_text(&*payload), "boom");

    let payload: Box<dyn Any + Send> = Box::new("owned boom".to_string());
    assert_eq!(panic_payload_text(&*payload), "owned boom");

    let payload: Box<dyn Any + Send> = Box::new(42_i32);
    assert_eq!(panic_payload_text(&*payload), "未知负载");
}
