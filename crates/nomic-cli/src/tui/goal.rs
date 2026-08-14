//! goal 模式自动追问：run 正常结束且仍有未完成 todo 时，以 user 消息
//! 追问模型继续完成。状态（与 agent 共享的 todo 清单、连续追问计数）与
//! 策略（上限、清零时机、提示词）集中于 [`GoalNudger`]，driver 只持有
//! 一个实例并在 run 结束时消费判定结果。

use nomic_tools::TodoStore;

/// goal 模式连续自动追问的次数上限：防止模型反复不收尾时失控循环
///（达到上限后暂停追问，用户手动继续或 `/goal` 重开后重新计数）。
const MAX_GOAL_NUDGES: u32 = 3;

/// goal 模式自动追问状态。
pub(super) struct GoalNudger {
    /// todo 清单（与 agent 的 todo 工具共享；追问判定用）
    todos: TodoStore,
    /// 连续自动追问次数（用户提交新 prompt 或 run 异常结束时清零）
    nudges: u32,
}

/// 一次追问判定结果（run 结束时由 driver 消费）。
pub(super) enum Nudge {
    /// 不追问（goal 关闭 / run 异常结束 / 清单已全部完成）：计数已清零
    Quiet,
    /// 追问：附提示词（作为 user 消息提交；计数已 +1）
    Remind(String),
    /// 连续追问达上限：暂停追问（计数已清零），附提示
    Capped(String),
}

impl GoalNudger {
    pub(super) const fn new(todos: TodoStore) -> Self {
        Self { todos, nudges: 0 }
    }

    /// 清零连续追问计数（用户主动提交新 prompt、run 异常结束、队列接管时）。
    pub(super) const fn reset(&mut self) {
        self.nudges = 0;
    }

    /// run 结束后的追问判定。`eligible` 为「run 正常结束且 goal 模式开启」；
    /// 不追问与达上限时计数清零，追问时计数 +1。
    pub(super) fn next(&mut self, eligible: bool) -> Nudge {
        if !eligible {
            self.nudges = 0;
            return Nudge::Quiet;
        }
        let Some(prompt) = reminder_prompt(&self.todos) else {
            self.nudges = 0;
            return Nudge::Quiet;
        };
        if self.nudges >= MAX_GOAL_NUDGES {
            self.nudges = 0;
            return Nudge::Capped(format!(
                "goal 模式：已连续追问 {MAX_GOAL_NUDGES} 次，todo 仍未全部完成，\
                 暂停自动追问（手动继续或 /goal 重开）。"
            ));
        }
        self.nudges += 1;
        Nudge::Remind(prompt)
    }
}

/// goal 模式的追问提示词：列出未完成的 todo（pending / in_progress），
/// 要求模型继续完成；清单为空或没有未完成项时返回 `None`（不追问）。
///
/// 该文本作为 user 消息进入对话历史（聊天区可见、随 session 落库）。
fn reminder_prompt(todos: &TodoStore) -> Option<String> {
    let incomplete = todos.incomplete();
    if incomplete.is_empty() {
        return None;
    }
    Some(format!(
        "[goal 模式] react loop 已停止，但 todo 清单还有未完成的任务：\n{}\n\
         请继续完成上述剩余任务：逐项推进，完成后立即用 todo_write 更新状态；全部完成前不要停止。",
        nomic_tools::render_todos(&incomplete)
    ))
}

#[cfg(test)]
mod tests {
    use nomic_core::AgentTool;
    use nomic_tools::{TodoItemInput, TodoStatus, TodoStore, TodoWriteTool};

    use super::{GoalNudger, MAX_GOAL_NUDGES, Nudge};

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

    fn item(title: &str, status: TodoStatus) -> TodoItemInput {
        TodoItemInput {
            id: None,
            title: title.to_string(),
            status,
            children: Vec::new(),
        }
    }

    /// 追问提示词：列出未完成 todo；空清单或全部完成（含已取消）时不追问。
    #[tokio::test]
    async fn reminder_lists_incomplete_todos_only() {
        let store = TodoStore::new();
        let nudger = GoalNudger::new(store.clone());

        // 空清单：不追问
        assert!(reminder_outcome(&nudger).is_none());

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
        let prompt = reminder_outcome(&nudger).expect("有未完成项应追问");
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
        assert!(reminder_outcome(&nudger).is_none());
    }

    /// eligible 时的追问提示词（不推进计数；纯提示词断言用）。
    fn reminder_outcome(nudger: &GoalNudger) -> Option<String> {
        super::reminder_prompt(&nudger.todos)
    }

    /// 追问计数：eligibility 关闭或清单完成时清零；连续追问达上限后
    /// 暂停（Capped）并清零，重新计数。
    #[tokio::test]
    async fn nudge_counts_up_to_cap_then_pauses() {
        let store = TodoStore::new();
        write(&store, vec![item("未完成", TodoStatus::Pending)]).await;
        let mut nudger = GoalNudger::new(store);

        // 连续追问 MAX_GOAL_NUDGES 次
        for round in 1..=MAX_GOAL_NUDGES {
            let Nudge::Remind(prompt) = nudger.next(true) else {
                panic!("第 {round} 次应追问");
            };
            assert!(prompt.contains("未完成"), "{prompt}");
        }
        // 达上限：暂停并提示
        let Nudge::Capped(notice) = nudger.next(true) else {
            panic!("达上限应暂停追问");
        };
        assert!(notice.contains(&MAX_GOAL_NUDGES.to_string()), "{notice}");
        // 计数已清零：下一轮重新追问
        assert!(matches!(nudger.next(true), Nudge::Remind(_)));

        // run 异常结束 / goal 关闭（eligible=false）：不追问且清零
        nudger.reset();
        assert!(matches!(nudger.next(false), Nudge::Quiet));
        assert!(matches!(nudger.next(true), Nudge::Remind(_)));
    }
}
