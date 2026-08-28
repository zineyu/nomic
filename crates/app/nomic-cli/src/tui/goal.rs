//! goal 命令的目标驱动运行：`goal <目标>` 启动后，run 正常结束而 agent
//! 尚未调用 `goal_done` 时，以 user 消息复述目标与要求继续追问，直到目标
//! 完成。状态（与 `goal_done` 工具共享的 [`GoalSession`]、连续追问计数）与
//! 策略（上限、清零时机、提示词）集中于 [`GoalNudger`]，driver 只持有一个
//! 实例并在 run 结束时消费判定结果。

use std::sync::Arc;

use nomic_tools::GoalSession;

/// 连续自动追问的次数上限：防止模型反复不收尾时失控循环（达到上限后
/// 暂停追问，目标仍进行中——用户手动继续后重新计数，或 `goal` 无参取消）。
const MAX_GOAL_NUDGES: u32 = 3;

/// goal 模式自动追问状态。
pub(super) struct GoalNudger {
    /// 进行中的目标会话（与 agent 的 `goal_done` 工具共享同一句柄；
    /// `None` = 未在目标驱动运行中）
    session: Option<Arc<GoalSession>>,
    /// 连续自动追问次数（用户提交新 prompt、run 异常结束、队列接管时清零）
    nudges: u32,
}

/// 一次追问判定结果（run 结束时由 driver 消费）。
pub(super) enum Nudge {
    /// 不追问（无进行中目标 / run 异常结束）：计数已清零
    Quiet,
    /// 目标完成（agent 已调用 `goal_done`）：driver 换回正常工具集
    ///（会话已随判定解除）
    Done,
    /// 追问：附复述目标的提示词（作为 user 消息提交；计数已 +1）
    Remind(String),
    /// 连续追问达上限：暂停追问（计数清零，目标仍进行中），附提示
    Capped(String),
}

impl GoalNudger {
    pub(super) const fn new() -> Self {
        Self {
            session: None,
            nudges: 0,
        }
    }

    /// 启动 / 替换目标会话（`goal <目标>`；计数清零）。
    pub(super) fn arm(&mut self, session: Arc<GoalSession>) {
        self.session = Some(session);
        self.nudges = 0;
    }

    /// 取消进行中的目标（`goal` 无参、会话切换）。
    pub(super) fn disarm(&mut self) {
        self.session = None;
        self.nudges = 0;
    }

    /// 清零连续追问计数（用户主动提交新 prompt、run 异常结束、队列接管时）。
    pub(super) const fn reset(&mut self) {
        self.nudges = 0;
    }

    /// run 结束后的追问判定。`eligible` 为「run 正常结束」；目标已完成
    /// 时无视 eligibility 直接判定为 [`Nudge::Done`]（goal_done 落账即算
    /// 完成）；不追问与达上限时计数清零，追问时计数 +1。
    pub(super) fn next(&mut self, eligible: bool) -> Nudge {
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
                 暂停自动追问（发送消息手动继续，或 goal 无参取消目标）。"
            ));
        }
        self.nudges += 1;
        Nudge::Remind(reminder_prompt(session.objective()))
    }
}

/// `goal <目标>` 的启动提示词：把命令内容包装为「目标驱动运行」指令，
/// 作为 user 消息提交（聊天区可见、随 session 落库）。
pub(super) fn goal_prompt(objective: &str) -> String {
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
    use super::{GoalNudger, GoalSession, MAX_GOAL_NUDGES, Nudge, goal_prompt};

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
        assert!(matches!(nudger.next(true), Nudge::Quiet));
    }

    /// 无进行中目标 / 取消目标后：不追问。
    #[test]
    fn disarmed_nudger_stays_quiet() {
        let mut nudger = GoalNudger::new();
        assert!(matches!(nudger.next(true), Nudge::Quiet));
        nudger.arm(GoalSession::new("目标"));
        nudger.disarm();
        assert!(matches!(nudger.next(true), Nudge::Quiet));
    }
}
