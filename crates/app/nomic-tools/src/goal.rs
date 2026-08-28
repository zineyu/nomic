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

/// 连续自动追问的次数上限：防止模型反复不收尾时失控循环（达到上限后
/// 暂停追问，目标仍进行中——用户手动继续后重新计数，或取消目标）。
const MAX_GOAL_NUDGES: u32 = 3;

/// goal 模式自动追问状态（TUI / web 共用，追问语义的唯一口径）。
///
/// run 正常结束而 agent 尚未调用 `goal_done` 时，交互端以 user 消息复述
/// 目标与要求继续追问，直到目标完成。状态（与 `goal_done` 工具共享的
/// [`GoalSession`]、连续追问计数）与策略（上限、清零时机、提示词）集中
/// 于本类型，交互端只持有一个实例并在 run 结束时消费判定结果（与
/// [`QuestionRegistry`](crate::QuestionRegistry) 的共享生命周期先例同构）。
#[derive(Debug, Default)]
pub struct GoalNudger {
    /// 进行中的目标会话（与 agent 的 `goal_done` 工具共享同一句柄；
    /// `None` = 未在目标驱动运行中）
    session: Option<Arc<GoalSession>>,
    /// 连续自动追问次数（用户提交新 prompt、run 异常结束、队列接管时清零）
    nudges: u32,
}

/// 一次追问判定结果（run 结束时由交互端消费）。
#[derive(Debug)]
pub enum Nudge {
    /// 不追问（无进行中目标 / run 异常结束）：计数已清零
    Quiet,
    /// 目标完成（agent 已调用 `goal_done`）：交互端换回正常工具集
    ///（会话已随判定解除）
    Done,
    /// 追问：附复述目标的提示词（作为 user 消息提交；计数已 +1）
    Remind(String),
    /// 连续追问达上限：暂停追问（计数清零，目标仍进行中），附提示
    Capped(String),
}

impl GoalNudger {
    /// 创建空闲追问器（无进行中目标）。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            session: None,
            nudges: 0,
        }
    }

    /// 启动 / 替换目标会话（`goal <目标>`；计数清零）。
    pub fn arm(&mut self, session: Arc<GoalSession>) {
        self.session = Some(session);
        self.nudges = 0;
    }

    /// 取消进行中的目标（无参 goal 命令、会话切换）。
    pub fn disarm(&mut self) {
        self.session = None;
        self.nudges = 0;
    }

    /// 清零连续追问计数（用户主动提交新 prompt、run 异常结束、队列接管时）。
    pub const fn reset(&mut self) {
        self.nudges = 0;
    }

    /// 进行中的目标会话（状态徽标与测试观察用）。
    pub const fn session(&self) -> Option<&Arc<GoalSession>> {
        self.session.as_ref()
    }

    /// run 结束后的追问判定。`eligible` 为「run 正常结束」；目标已完成
    /// 时无视 eligibility 直接判定为 [`Nudge::Done`]（goal_done 落账即算
    /// 完成）；不追问与达上限时计数清零，追问时计数 +1。
    pub fn next(&mut self, eligible: bool) -> Nudge {
        let Some(session) = &self.session else {
            return Nudge::Quiet;
        };
        if session.is_done() {
            self.disarm();
            return Nudge::Done;
        }
        if !eligible {
            self.nudges = 0;
            return Nudge::Quiet;
        }
        if self.nudges >= MAX_GOAL_NUDGES {
            self.nudges = 0;
            return Nudge::Capped(format!(
                "goal：已连续追问 {MAX_GOAL_NUDGES} 次，目标仍未完成（未调用 goal_done），\
                 暂停自动追问（发送消息手动继续，或取消目标）。"
            ));
        }
        self.nudges += 1;
        Nudge::Remind(reminder_prompt(session.objective()))
    }
}

/// `goal <目标>` 的启动提示词：把命令内容包装为「目标驱动运行」指令，
/// 作为 user 消息提交（聊天区可见、随 session 落库）。
#[must_use]
pub fn goal_prompt(objective: &str) -> String {
    format!(
        "用户为你设定了一个目标：\n\n<goal>\n{objective}\n</goal>\n\n\
         从现在起，把上述内容作为你的最高优先级任务，持续工作直到目标完成：\n\
         - 自主拆解目标、逐项推进，必要时用 todo_write 维护任务清单跟踪进度；\n\
         - 不要向用户提问（ask_user_question 当前不可用）：信息不足时按最合理的假设\
         继续推进，并在最终汇报中说明所作假设；\n\
         - 不要中途停下等待指示——每轮结束前自查目标是否已完全达成，未达成就继续工作；\n\
         - 确认目标完全达成后，调用 goal_done 工具并附上完成情况总结。\
         只有调用了 goal_done，目标才算完成。"
    )
}

/// 追问提示词：复述目标与要求。该文本作为 user 消息进入对话历史
///（聊天区可见、随 session 落库）。
fn reminder_prompt(objective: &str) -> String {
    format!(
        "[goal] react loop 已停止，但目标尚未完成（你还没有调用 goal_done 工具）。\n\
         复述目标：\n\n<goal>\n{objective}\n</goal>\n\n\
         要求：持续工作直到目标完全达成；不要向用户提问（ask_user_question 不可用）；\
         不要中途停下等待指示。确认目标完全达成后，调用 goal_done 工具汇报。请继续。"
    )
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

    /// 启动提示词：包裹目标原文，并声明 goal_done 收尾与禁止提问的要求。
    #[test]
    fn goal_prompt_wraps_objective_with_requirements() {
        let prompt = goal_prompt("修复全部失败测试");
        assert!(
            prompt.contains("<goal>\n修复全部失败测试\n</goal>"),
            "{prompt}"
        );
        assert!(prompt.contains("goal_done"), "{prompt}");
        assert!(prompt.contains("ask_user_question"), "{prompt}");
    }

    /// 追问提示词：复述目标原文与要求。
    #[test]
    fn reminder_restates_objective_and_requirements() {
        let mut nudger = GoalNudger::new();
        nudger.arm(GoalSession::new("实现用户登录"));
        let Nudge::Remind(prompt) = nudger.next(true) else {
            panic!("目标未完成应追问");
        };
        assert!(prompt.contains("<goal>\n实现用户登录\n</goal>"), "{prompt}");
        assert!(prompt.contains("goal_done"), "{prompt}");
    }

    /// 追问计数：run 异常结束（eligible=false）不追问且清零；连续追问达
    /// 上限后暂停（Capped）并清零、目标仍进行中（下一轮重新追问）。
    #[test]
    fn nudge_counts_up_to_cap_then_pauses() {
        let mut nudger = GoalNudger::new();
        nudger.arm(GoalSession::new("目标"));

        for round in 1..=MAX_GOAL_NUDGES {
            let Nudge::Remind(prompt) = nudger.next(true) else {
                panic!("第 {round} 次应追问");
            };
            assert!(prompt.contains("目标"), "{prompt}");
        }
        let Nudge::Capped(notice) = nudger.next(true) else {
            panic!("达上限应暂停追问");
        };
        assert!(notice.contains(&MAX_GOAL_NUDGES.to_string()), "{notice}");
        // 计数已清零、目标仍进行中：下一轮重新追问
        assert!(matches!(nudger.next(true), Nudge::Remind(_)));

        // run 异常结束：不追问且清零
        nudger.reset();
        assert!(matches!(nudger.next(false), Nudge::Quiet));
        assert!(matches!(nudger.next(true), Nudge::Remind(_)));
    }

    /// 目标完成（goal_done 落账）：无视 eligibility 判定为 Done 并解除
    /// 会话（后续判定为 Quiet）。
    #[test]
    fn done_disarms_goal_regardless_of_eligibility() {
        let session = GoalSession::new("目标");
        let mut nudger = GoalNudger::new();
        nudger.arm(session.clone());

        session.mark_done();
        assert!(matches!(nudger.next(false), Nudge::Done));
        assert!(nudger.session().is_none(), "Done 后会话应解除");
        assert!(matches!(nudger.next(true), Nudge::Quiet));
    }

    /// 无进行中目标 / 取消目标后：不追问。
    #[test]
    fn disarmed_nudger_stays_quiet() {
        let mut nudger = GoalNudger::new();
        assert!(matches!(nudger.next(true), Nudge::Quiet));
        nudger.arm(GoalSession::new("目标"));
        assert!(nudger.session().is_some(), "arm 后会话应在场");
        nudger.disarm();
        assert!(matches!(nudger.next(true), Nudge::Quiet));
    }
}
