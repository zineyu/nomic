//! `goal_done` 工具与 goal 模式共享会话状态。
//!
//! goal 模式（TUI `goal <目标>` 命令）以目标驱动运行：启动时把目标包装为
//! 提示词提交，并把 agent 工具集换为「去掉 `ask_user_question`（目标驱动
//! 运行不与用户交互）+ 加入 `goal_done`」（见 [`goal_tools`]）；agent 确认
//! 目标完成后调用 `goal_done` 汇报，工具经共享的 [`GoalSession`] 标记完成，
//! 交互端据此停止「run 停止时复述目标」的自动追问并换回正常工具集。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use nomic_core::{AgentTool, DynTool, ToolError, ToolResult, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

/// 一次 goal 模式运行的共享会话状态：`goal_done` 工具（agent 任务内）与
/// 交互端（run 结束后的追问判定）经同一句柄观察目标是否完成。
#[derive(Debug)]
pub struct GoalSession {
    /// 目标原文（启动提示词与追问提示词的复述内容）
    objective: String,
    /// 目标已完成（agent 已调用 `goal_done`）
    done: AtomicBool,
}

impl GoalSession {
    /// 以目标原文创建会话（未完成态）。
    pub fn new(objective: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            objective: objective.into(),
            done: AtomicBool::new(false),
        })
    }

    /// 目标原文。
    pub fn objective(&self) -> &str {
        &self.objective
    }

    /// 标记目标完成（`goal_done` 工具执行时调用；幂等）。
    pub fn mark_done(&self) {
        self.done.store(true, Ordering::Relaxed);
    }

    /// 目标是否已完成。
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }
}

/// `goal_done` 参数。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GoalDoneParams {
    /// A concise summary of what was accomplished to meet the goal
    pub summary: String,
}

const TOOL_NAME: &str = "goal_done";
const TOOL_DESCRIPTION: &str = "Report that the goal assigned by the user has been fully \
         accomplished. Only call this when every requirement of the goal is actually met \
         (verified by builds/tests where applicable) — the current run ends right after \
         this call. Provide a concise summary of what was done.";

/// `goal_done` 工具：标记共享会话完成并终止本轮运行（`terminate`）。
#[derive(Debug, Clone)]
pub struct GoalDoneTool {
    session: Arc<GoalSession>,
}

impl GoalDoneTool {
    /// 绑定本次 goal 模式运行的共享会话。
    pub const fn new(session: Arc<GoalSession>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl AgentTool for GoalDoneTool {
    type Params = GoalDoneParams;

    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn label(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
    }

    async fn execute(
        &self,
        params: Self::Params,
        _cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        let summary = params.summary.trim();
        if summary.is_empty() {
            return Err(ToolError::new("summary must not be empty"));
        }
        self.session.mark_done();
        Ok(ToolResult {
            details: Some(serde_json::json!({ "summary": summary })),
            terminate: true,
            ..ToolResult::text(format!("Goal marked as done. Summary: {summary}"))
        })
    }
}

/// goal 模式的工具集变换：在基础工具集上去掉 `ask_user_question`（目标
/// 驱动运行不与用户交互），加入绑定 `session` 的 `goal_done`（完成汇报
/// 通道）。`DynTool` 是 `Arc` 共享句柄，克隆廉价。
pub fn goal_tools(base: &[DynTool], session: &Arc<GoalSession>) -> Vec<DynTool> {
    base.iter()
        .filter(|tool| tool.name() != "ask_user_question")
        .cloned()
        .chain(std::iter::once(DynTool::new(GoalDoneTool::new(
            session.clone(),
        ))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn execute(tool: &GoalDoneTool, summary: &str) -> Result<ToolResult, ToolError> {
        tool.execute(
            GoalDoneParams {
                summary: summary.to_string(),
            },
            CancellationToken::new(),
            Box::new(|_| {}),
        )
        .await
    }

    /// goal_done 标记共享会话完成、终止本轮运行，并回传汇报文本；
    /// 重复调用幂等。
    #[tokio::test]
    async fn goal_done_marks_session_and_terminates() {
        let session = GoalSession::new("修复全部失败测试");
        let tool = GoalDoneTool::new(session.clone());
        assert!(!session.is_done());

        let result = execute(&tool, "已修复 3 个失败测试")
            .await
            .expect("执行应成功");
        assert!(session.is_done(), "会话应标记完成");
        assert!(result.terminate, "goal_done 后本轮运行应终止");
        let nomic_ai::UserContent::Text(text) = &result.content[0] else {
            panic!("expected text result");
        };
        assert!(text.text.contains("已修复 3 个失败测试"), "{}", text.text);
        assert_eq!(
            result.details.expect("details")["summary"],
            "已修复 3 个失败测试"
        );

        execute(&tool, "再次汇报").await.expect("重复调用应成功");
    }

    /// 空汇报拒绝（转为错误结果回喂模型自我修正），且不标记完成。
    #[tokio::test]
    async fn blank_summary_rejected() {
        let session = GoalSession::new("目标");
        let tool = GoalDoneTool::new(session.clone());
        let error = execute(&tool, "   ").await.expect_err("空汇报必须报错");
        assert!(error.to_string().contains("summary must not be empty"));
        assert!(!session.is_done(), "校验失败不应标记完成");
    }

    /// goal 工具集变换：去掉 ask_user_question、加入绑定同一会话的
    /// goal_done，其余工具保持原序。
    #[tokio::test]
    async fn goal_tools_swaps_ask_for_goal_done() {
        struct NoopSink;

        #[async_trait]
        impl crate::QuestionSink for NoopSink {
            async fn ask(
                &self,
                _question: crate::AskUserQuestion,
                _cancel: CancellationToken,
            ) -> Result<crate::AskUserAnswer, ToolError> {
                unreachable!("测试不提问")
            }
        }

        let base = vec![
            DynTool::new(crate::ReadTool::new()),
            DynTool::new(crate::AskUserQuestionTool::new(Arc::new(NoopSink))),
        ];
        let session = GoalSession::new("目标");
        let tools = goal_tools(&base, &session);

        let names: Vec<&str> = tools.iter().map(DynTool::name).collect();
        assert_eq!(names, ["read", "goal_done"]);

        // 变换后的 goal_done 绑定的是传入的同一句柄
        let goal_done = tools
            .iter()
            .find(|tool| tool.name() == "goal_done")
            .expect("goal_done 应在工具集中");
        goal_done
            .execute(
                serde_json::json!({"summary": "完成"}),
                CancellationToken::new(),
                Box::new(|_| {}),
            )
            .await
            .expect("执行应成功");
        assert!(session.is_done(), "工具与交互端应共享同一会话");
    }
}
