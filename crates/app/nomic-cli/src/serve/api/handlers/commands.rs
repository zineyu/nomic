//! 斜杠命令解析与 goal 命令处置（`/compact` / `/continue` / `/goal`；
//! 800 行封顶拆分，与 [`crud`](super::crud) 同一模式）。

use std::sync::Arc;

use super::super::ApiError;
use crate::serve::{AppState, SessionRuntime};

/// 斜杠命令的解析结果：runner job 类直接提交串行队列；goal 命令需要
/// 换工具集与登记目标会话，由 handler 单独处置。
#[derive(Debug)]
pub enum SlashCommand {
    Job(nomic_core::SessionJob),
    Goal(Option<String>),
}

/// 解析斜杠命令体（已去掉前导 `/`）；未知命令或参数非法时返回带用法
/// 提示的错误。web 支持的命令子集：`/compact [聚焦指令]`、`/continue`、
/// `/goal <目标>`（`/goal` 无参取消进行中的目标）。
pub fn parse_slash_command(rest: &str) -> Result<SlashCommand, ApiError> {
    const USAGE: &str = "available commands: /compact [focus instruction] (compress context), /continue (resume last run), /goal <objective> (goal-driven run; bare /goal cancels)";
    // 命令名取到首个 `:` 或空白为止；其余部分为参数（`compact 指令` 与
    // `compact:指令` 两种形式等价，冒号形式与 TUI 命令语法对齐）
    let (name, arg) = match rest.find(|c: char| c == ':' || c.is_whitespace()) {
        Some(index) => {
            let (name, tail) = rest.split_at(index);
            let delimiter = tail.chars().next().expect("find 命中必有字符");
            (
                name,
                Some(tail[delimiter.len_utf8()..].trim()).filter(|arg| !arg.is_empty()),
            )
        }
        None => (rest, None),
    };
    match name {
        "compact" => Ok(SlashCommand::Job(nomic_core::SessionJob::Compact {
            instructions: arg.map(str::to_string),
        })),
        "continue" if arg.is_none() => Ok(SlashCommand::Job(nomic_core::SessionJob::Continue)),
        // 目标原文是自由文本（可含空格）；无参 = 取消进行中的目标
        "goal" => Ok(SlashCommand::Goal(arg.map(str::to_string))),
        _ => Err(ApiError::BadRequest(format!("未知命令 /{rest}。{USAGE}"))),
    }
}

/// `/goal` 命令：`Some(目标)` 启动目标驱动运行（空闲时方可启动，目标
/// 经 mention 展开后交给 [`SessionRuntime::start_goal`]）；`None` 取消
/// 进行中的目标（运行中亦可，追问立即解除、工具集在本轮结束后换回）。
pub fn goal_command(
    state: &AppState,
    session: &Arc<SessionRuntime>,
    objective: Option<String>,
) -> Result<(), ApiError> {
    match objective {
        Some(objective) => {
            if session.runner.is_running() {
                return Err(ApiError::BadRequest(
                    "运行中无法启动目标：/goal <目标> 须等本轮结束".to_string(),
                ));
            }
            let objective = crate::mention::expand_mentions(
                &objective,
                &state.inner.factory.skill_resolver,
                &session.project,
            );
            session.start_goal(objective)
        }
        None => {
            if session.cancel_goal() {
                Ok(())
            } else {
                Err(ApiError::BadRequest("当前没有进行中的目标。".to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! 斜杠命令解析与 `/goal` 命令路径（goal 集成测试经 handle_prompt 走
    //! 完整提交路径）。

    use super::super::handle_prompt;
    use super::*;
    use crate::serve::ServerEvent;

    #[test]
    fn parse_slash_command_compact_and_continue() {
        assert!(matches!(
            parse_slash_command("compact").expect("compact"),
            SlashCommand::Job(nomic_core::SessionJob::Compact { instructions: None })
        ));
        // 自由文本指令（空格形式）
        let Ok(SlashCommand::Job(nomic_core::SessionJob::Compact {
            instructions: Some(instructions),
        })) = parse_slash_command("compact 专注 测试 部分")
        else {
            panic!("compact 带指令应解析成功");
        };
        assert_eq!(instructions, "专注 测试 部分");
        // 冒号形式（与 TUI 命令语法对齐）
        let Ok(SlashCommand::Job(nomic_core::SessionJob::Compact {
            instructions: Some(instructions),
        })) = parse_slash_command("compact:focus on tests")
        else {
            panic!("compact:指令 应解析成功");
        };
        assert_eq!(instructions, "focus on tests");
        assert!(matches!(
            parse_slash_command("continue").expect("continue"),
            SlashCommand::Job(nomic_core::SessionJob::Continue)
        ));
    }

    #[test]
    fn parse_slash_command_goal_takes_free_text_objective() {
        // 无参 = 取消进行中的目标
        assert!(matches!(
            parse_slash_command("goal").expect("goal"),
            SlashCommand::Goal(None)
        ));
        // 目标原文是自由文本（可含空格）
        let Ok(SlashCommand::Goal(Some(objective))) = parse_slash_command("goal 修复 全部 测试")
        else {
            panic!("goal 带目标应解析成功");
        };
        assert_eq!(objective, "修复 全部 测试");
        // 冒号形式同样接受
        let Ok(SlashCommand::Goal(Some(objective))) = parse_slash_command("goal:fix all tests")
        else {
            panic!("goal:目标 应解析成功");
        };
        assert_eq!(objective, "fix all tests");
    }

    #[test]
    fn parse_slash_command_rejects_unknown_and_invalid_usage() {
        let Err(ApiError::BadRequest(message)) = parse_slash_command("quit") else {
            panic!("未知命令应报错");
        };
        assert!(message.contains("/compact"), "{message}");
        assert!(message.contains("/continue"), "{message}");
        // continue 不接受参数
        assert!(parse_slash_command("continue extra").is_err());
        // 空命令名
        assert!(parse_slash_command("").is_err());
    }

    /// `/goal <目标>` 空闲启动：登记目标会话并广播 goal_changed，包装
    /// 提示词作为 prompt 提交（测试在 job 出队前取消，不发起真实请求）。
    #[tokio::test]
    async fn goal_command_arms_session_and_broadcasts() {
        let (state, session_id) = crate::serve::tests::test_state_with_session().await;
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .get(&session_id)
            .expect("session")
            .clone();
        let mut events = state.inner.events.subscribe();

        let ack = handle_prompt(
            &state,
            &session_id,
            "/goal 修复测试".to_string(),
            Vec::new(),
        )
        .await;
        let ServerEvent::PromptAck { queued, .. } = ack else {
            panic!("应返回 PromptAck");
        };
        assert!(!queued, "空闲启动不应标记排队");
        assert_eq!(session.goal_objective().as_deref(), Some("修复测试"));
        assert!(
            matches!(
                events.try_recv().expect("goal_changed"),
                ServerEvent::GoalChanged {
                    status: crate::serve::GoalStatus::Started,
                    objective: Some(objective),
                    ..
                } if objective == "修复测试"
            ),
            "启动应广播 goal_changed"
        );
        // 取消在途 job，避免测试发起真实 provider 请求
        session.cancel_run();

        // `/goal` 无参取消：解除追问状态并广播 cancelled
        let ack = handle_prompt(&state, &session_id, "/goal".to_string(), Vec::new()).await;
        assert!(matches!(ack, ServerEvent::PromptAck { .. }));
        assert_eq!(session.goal_objective(), None);
        assert!(matches!(
            events.try_recv().expect("goal_changed"),
            ServerEvent::GoalChanged {
                status: crate::serve::GoalStatus::Cancelled,
                ..
            }
        ));

        // 无进行中目标时 `/goal` 无参报错
        let event = handle_prompt(&state, &session_id, "/goal".to_string(), Vec::new()).await;
        let ServerEvent::Error { message, .. } = event else {
            panic!("无目标时取消应返回 error 事件");
        };
        assert!(message.contains("没有进行中的目标"), "{message}");
    }

    /// `/goal <目标>` 运行中拒绝（与 TUI「启动属会话命令」同一口径）。
    #[tokio::test]
    async fn goal_command_rejected_while_running() {
        let (state, session_id) = crate::serve::tests::test_state_with_session().await;
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .get(&session_id)
            .expect("session")
            .clone();
        // 用一个 runner job 占住运行态（/continue 空历史立即结束；单线程
        // 测试运行时中 submit 与下方判定之间无调度点）
        session
            .runner
            .submit(nomic_core::SessionJob::Continue)
            .expect("submit continue");

        let event =
            handle_prompt(&state, &session_id, "/goal 新目标".to_string(), Vec::new()).await;
        let ServerEvent::Error { message, .. } = event else {
            panic!("运行中启动目标应返回 error 事件");
        };
        assert!(message.contains("运行中"), "{message}");
        assert!(session.goal_objective().is_none(), "拒绝后不应登记目标");
        session.cancel_run();
    }
}
