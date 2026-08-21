//! agent driver：薄适配层——agent 本体由 core 的 actor 任务持有
//! （[`Agent::spawn`]，ADR-0022），run 类 job（prompt / 压缩 / 续跑）的
//! 串行消费、取消与生命周期翻译由 core 的 [`SessionRunner`] 持有
//! （ADR-0033）；driver 只保留 TUI 侧适配：goal 模式追问与消息队列
//! （ADR-0014）、事件循环的唤醒处理（[`handle_wake`] / [`handle_prompt_done`] /
//! [`next_wake`]）与按键映射（[`map_key`]）、Effect 外部资源接线
//! （[`execute_effect`]）。注入 / 清空 / 恢复 / 模型切换等 fire-and-forget
//! 变更不是 runner job，经 [`AgentHandle`] 直调（邮箱 FIFO 保证其先于
//! 紧随的 job 生效）。

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures::StreamExt as _;
use nomic_ai::{Model, ThinkingLevel};
use nomic_core::{
    Agent, AgentEvent, AgentHandle, CompactOutcome, ContinueOutcome, JobOutcome,
    NOTHING_TO_COMPACT, NOTHING_TO_CONTINUE, PromptOutcome, RunnerEvent, SessionJob, SessionRunner,
};
use nomic_session::SessionRecorder;
use nomic_skills::SkillResolver;
use nomic_tools::TodoStore;
use tokio::sync::mpsc;

use super::app::{App, Effect, Key, SkillEntry};
use super::ask::PendingQuestion;
use super::effects::{self, ModelSwitcher, SessionBinding};
use super::goal::{GoalNudger, Nudge};
use super::terminal::edit_input_in_editor;
use super::{TuiTerminal, panic_payload_text};
use crate::mention;
use crate::model::ModelResolver;

/// 启动 agent driver：agent 经 [`Agent::spawn`] 移入 core actor 任务，run 类
/// job 经 [`SessionRunner::spawn`] 移入 core runner 任务（串行消费、取消与
/// 生命周期翻译收在其中）；driver 本体只是事件循环持有的接线端资源。
// 参数均为 driver 的独立组成部分，打包为参数结构只会增加间接层
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_driver(
    agent: Agent,
    recorder: Option<SessionRecorder>,
    base: nomic_tools::BaseDir,
    models: ModelResolver,
    model: Model,
    skill_resolver: SkillResolver,
    reasoning: Option<ThinkingLevel>,
    todos: TodoStore,
) -> (Driver, mpsc::UnboundedReceiver<RunnerEvent>) {
    let (handle, actor_task) = agent.spawn();
    let (runner, runner_events, runner_task) = SessionRunner::spawn(handle.clone());
    let driver = Driver {
        runner,
        handle,
        pending_question: None,
        actor_task: Some(actor_task),
        runner_task: Some(runner_task),
        alive: true,
        session: SessionBinding::new(recorder, base),
        model: ModelSwitcher::new(models, model, reasoning),
        skill_resolver,
        goal: GoalNudger::new(todos),
    };
    (driver, runner_events)
}

/// 事件循环持有的驱动端资源。字段全私有（ADR-0024）：业务状态按关注点
/// 收在子结构（[`SessionBinding`] / [`ModelSwitcher`] / [`GoalNudger`]），
/// effects 函数经 [`execute_effect`] 转发拿到对应子结构，字段表不再是 interface。
pub(super) struct Driver {
    /// run 类 job 的提交端（prompt / 压缩 / 续跑；串行消费与取消令牌
    /// 管理收在 core 的 runner）
    runner: SessionRunner,
    /// actor 句柄：注入 / 清空 / 恢复 / 模型切换等 fire-and-forget 变更
    /// 直调（邮箱 FIFO 保证其先于紧随的 runner job 生效）
    handle: AgentHandle,
    /// 在途问题的回答回传端（提问弹层打开期间持有；作答/取消/中断
    /// 时发送或丢弃——丢弃即关闭通道，工具侧收到关闭转为错误结果）
    pending_question: Option<tokio::sync::oneshot::Sender<nomic_tools::AskUserAnswer>>,
    /// agent actor 任务的 JoinHandle：任务退出时取出详情转为 TUI 内错误提示
    actor_task: Option<tokio::task::JoinHandle<()>>,
    /// runner 任务的 JoinHandle（与 actor 任务任一退出即整体不可用，
    /// [`driver_failed`] 等待先结束的一方并中止另一方）
    runner_task: Option<tokio::task::JoinHandle<()>>,
    /// actor 是否存活；退出后其 channel 已关闭，事件循环跳过对应分支
    alive: bool,
    /// 会话落库绑定（recorder + cwd）：定稿点落库与父指针推进收在
    /// [`SessionRecorder`]（print 同一实现），`tree` 分支与 `new` /
    /// `resume` 的换绑收在 effects::session
    session: SessionBinding,
    /// 两步模型切换状态机（`models`）：当前模型/思考级别、待切换模型
    /// 与运行时解析器收在其中（effects::model 持有定义）
    model: ModelSwitcher,
    /// skill 解析器（ListSkills/LoadSkill 接线用，仅本文件访问）
    skill_resolver: SkillResolver,
    /// goal 模式自动追问（todo 清单与连续追问计数、上限与清零时机收在其中）
    goal: GoalNudger,
}

/// 事件循环单次等待的结果。
pub(super) enum Wake {
    /// 按键（Press/Repeat）
    Key(KeyEvent),
    /// 鼠标滚轮
    ScrollUp,
    ScrollDown,
    /// bracketed paste（终端粘贴/拖入的整段文本）
    Paste(String),
    /// agent 事件
    AgentEvent(AgentEvent),
    /// 提问弹层请求（`ask_user_question` 工具；回答通道由事件循环持有）
    UserQuestion(PendingQuestion),
    /// runner 事件（job 生命周期与执行结果）
    RunnerEvent(RunnerEvent),
    /// spinner 帧推进
    Tick,
    /// 仅需重绘（其他鼠标事件）
    Redraw,
    /// agent actor / runner 任务意外退出（panic 或提前返回），附详情
    DriverFailed(String),
    /// 终端事件流关闭：无法继续交互，退出循环
    TermClosed,
}

/// 处理一次唤醒；返回 `true` 表示终端事件流关闭、退出循环。
pub(super) async fn handle_wake(
    wake: Wake,
    app: &mut App,
    driver: &mut Driver,
    terminal: &mut TuiTerminal,
) -> bool {
    match wake {
        Wake::Key(key) => {
            // Ctrl+V 粘贴需异步读剪贴板，先于语义映射拦截
            if matches!(
                (key.code, key.modifiers),
                (KeyCode::Char('v'), KeyModifiers::CONTROL)
            ) {
                effects::paste_clipboard(app).await;
            } else if let Some(key) = map_key(key) {
                for effect in app.press(key) {
                    execute_effect(app, driver, terminal, effect).await;
                }
            }
        }
        Wake::ScrollUp => app.chat_mut().scroll_up(3),
        Wake::ScrollDown => app.chat_mut().scroll_down(3),
        Wake::Paste(text) => effects::handle_paste(app, &text),
        Wake::AgentEvent(event) => {
            // 落库策略收在 SessionRecorder（与 print 同一实现）：定稿点落库、
            // 父指针推进；失败仅提示不中断（store 非权威源）
            if let Err(error) = driver.session.record(&event).await {
                app.warn(format!("session 落库失败：{error}"));
            }
            app.handle_event(&event);
        }
        // 提问弹层请求：回答通道暂存在 driver（状态层不持有外部资源），
        // 用户在弹层作答后经 Effect 回传；Esc 取消/运行中断时丢弃
        Wake::UserQuestion(pending) => {
            driver.pending_question = Some(pending.answer_tx);
            app.open_question(pending.question);
        }
        Wake::RunnerEvent(event) => match event {
            // Started 服务需要合成运行生命周期的 adapter（web）；TUI 的
            // 运行状态在提交 job 时已由状态层 begin_run
            RunnerEvent::Started(_) => {}
            RunnerEvent::Finished(JobOutcome::Prompt(result)) => {
                handle_prompt_done(app, driver, terminal, result).await;
            }
            RunnerEvent::Finished(JobOutcome::Compact(result)) => {
                let notice = match result {
                    // 压缩成功经 CompactionEnd 事件渲染与落库，这里无需重复处理
                    Ok(CompactOutcome::Compacted(_)) => None,
                    Ok(CompactOutcome::NothingToCompact) => Some(NOTHING_TO_COMPACT.to_string()),
                    Err(error) => Some(format!("压缩失败，上下文保持不变：{error}")),
                };
                app.finish_run(notice);
            }
            RunnerEvent::Finished(JobOutcome::Continue(result)) => {
                // 续跑成功经事件流渲染与落库，这里无需重复处理
                app.finish_run(match result {
                    Ok(ContinueOutcome::Continued) => None,
                    Ok(ContinueOutcome::NothingToContinue) => Some(NOTHING_TO_CONTINUE.to_string()),
                    Err(error) => Some(format!("续跑失败：{error}")),
                });
            }
        },
        Wake::Tick => app.tick(),
        Wake::Redraw => {}
        Wake::DriverFailed(detail) => {
            tracing::error!(detail, "agent driver 任务意外退出");
            // 进行中的一轮永远不会回执：回到空闲态，避免 spinner 空转
            if app.is_running() {
                app.finish_run(None);
            }
            app.chat_mut().push_system(format!(
                "内部错误：agent 任务意外退出（{detail}）。对话记录仍可查看，但无法继续发送消息。"
            ));
        }
        Wake::TermClosed => return true,
    }
    false
}

/// 一轮 prompt 结束：队列（ADR-0014）优先于 goal 模式追问——
/// 队列非空时，正常结束即取出队首自动提交（QUEUE 模式打开期间冻结，
/// 退出 QUEUE 时恢复）；被取消/失败等异常结束则队列暂停保留，
/// 用户可空闲 Enter 或 Esc→m 恢复。队列为空时交给 [`GoalNudger`] 判定
/// goal 追问（追问提示词作为 user 消息提交；达到上限则暂停）。
pub(super) async fn handle_prompt_done(
    app: &mut App,
    driver: &mut Driver,
    terminal: &mut TuiTerminal,
    result: Result<PromptOutcome, nomic_core::ActorError>,
) {
    let end = match result {
        Ok(end) => end,
        Err(error) => {
            driver.goal.reset();
            app.finish_run(Some(format!("agent loop 失败：{error}")));
            return;
        }
    };
    if !app.queue().is_empty() {
        driver.goal.reset();
        if end.ended_normally() {
            app.finish_run(None);
            // QUEUE 模式打开时 drain 冻结（返回 None）：退出 QUEUE 时恢复
            if let Some(effect) = app.drain_queue() {
                execute_effect(app, driver, terminal, effect).await;
            }
        } else {
            app.finish_run(Some(format!(
                "运行未正常结束，队列保留 {} 条：空闲 Enter 发送下一条，Esc→m 编辑",
                app.queue().len()
            )));
        }
        return;
    }
    match driver.goal.next(end.ended_normally() && app.goal_mode()) {
        Nudge::Quiet => app.finish_run(None),
        Nudge::Capped(notice) => app.finish_run(Some(notice)),
        Nudge::Remind(reminder) => {
            if driver
                .runner
                .submit(SessionJob::Prompt {
                    text: reminder,
                    images: Vec::new(),
                })
                .is_ok()
            {
                app.begin_run();
            } else {
                app.finish_run(Some(
                    "内部错误：agent 任务已退出，goal 追问未发送。".to_string(),
                ));
            }
        }
    }
}

/// 等待下一个唤醒源：按键 / 鼠标 / agent 事件 / runner 事件 / spinner 帧。
///
/// agent 侧 channel 与 actor 任务同生命周期，runner 事件 channel 与 runner
/// 任务同生命周期：channel 关闭即任务退出，统一转为 [`Wake::DriverFailed`]。
pub(super) async fn next_wake(
    app: &App,
    driver: &mut Driver,
    term_events: &mut EventStream,
    spinner_ticker: &mut tokio::time::Interval,
    events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    runner_events: &mut mpsc::UnboundedReceiver<RunnerEvent>,
    question_rx: &mut mpsc::UnboundedReceiver<PendingQuestion>,
) -> Wake {
    let driver_alive = driver.alive;
    let running = app.is_running();
    tokio::select! {
        // spinner 动画仅在运行中推进；空闲时此分支永久挂起，不空转重绘
        () = async {
            if running {
                spinner_ticker.tick().await;
            } else {
                std::future::pending::<()>().await;
            }
        } => Wake::Tick,
        maybe_event = term_events.next() => match maybe_event {
            Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => Wake::Key(key),
            Some(Ok(Event::Paste(text))) => Wake::Paste(text),
            Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                MouseEventKind::ScrollUp => Wake::ScrollUp,
                MouseEventKind::ScrollDown => Wake::ScrollDown,
                _ => Wake::Redraw,
            },
            Some(Ok(_)) => Wake::Redraw,
            Some(Err(_)) | None => Wake::TermClosed,
        },
        // 提问弹层请求（工具侧在 agent 任务内推入；通道关闭即任务退出，
        // 已由 driver_failed 分支兜底；driver 退出后挂起避免空转）
        pending = async {
            if driver_alive {
                question_rx.recv().await
            } else {
                std::future::pending().await
            }
        } => match pending {
            Some(pending) => Wake::UserQuestion(pending),
            None => driver_failed(driver).await,
        },
        // driver 退出后 channel 已关闭，分支挂起避免立即返回 None 空转
        maybe_event = async {
            if driver_alive {
                events.recv().await
            } else {
                std::future::pending().await
            }
        } => match maybe_event {
            Some(event) => Wake::AgentEvent(event),
            None => driver_failed(driver).await,
        },
        maybe_runner_event = async {
            if driver_alive {
                runner_events.recv().await
            } else {
                std::future::pending().await
            }
        } => match maybe_runner_event {
            Some(event) => Wake::RunnerEvent(event),
            None => driver_failed(driver).await,
        },
    }
}

/// actor / runner 任务退出：任一退出即整体不可用（events 与 runner 事件
/// channel 的关闭都路由到这里）；等待先结束的一方取出详情（panic 负载等），
/// 中止另一方，转为 TUI 内提示。
pub(super) async fn driver_failed(driver: &mut Driver) -> Wake {
    driver.alive = false;
    let actor = driver.actor_task.take();
    let runner = driver.runner_task.take();
    let detail = match (actor, runner) {
        (Some(mut actor), Some(mut runner)) => {
            tokio::select! {
                result = &mut actor => {
                    runner.abort();
                    task_exit_detail("agent actor", result)
                }
                result = &mut runner => {
                    actor.abort();
                    task_exit_detail("runner", result)
                }
            }
        }
        // 已报告过一次（events 与 runner 事件两个 channel 先后关闭）
        _ => "任务已退出".to_string(),
    };
    Wake::DriverFailed(detail)
}

/// 任务退出详情：panic 负载、提前结束或 join 错误。
fn task_exit_detail(label: &str, result: Result<(), tokio::task::JoinError>) -> String {
    match result {
        Ok(()) => format!("{label}任务提前结束"),
        Err(error) if error.is_panic() => {
            let payload = error.into_panic();
            format!("{label}任务 panic：{}", panic_payload_text(&*payload))
        }
        Err(error) => format!("{label}任务错误：{error}"),
    }
}

/// 把 crossterm 按键映射为状态层的语义按键；未识别的组合返回 `None`。
pub(super) const fn map_key(key: KeyEvent) -> Option<Key> {
    Some(match (key.code, key.modifiers) {
        (KeyCode::Char(c), KeyModifiers::CONTROL) => Key::Ctrl(c),
        (KeyCode::Esc, _) => Key::Esc,
        (KeyCode::Tab, _) => Key::Tab,
        // 换行必须在提交之前匹配；依赖 kitty 键盘增强协议区分两者
        (KeyCode::Enter, KeyModifiers::SHIFT) => Key::Newline,
        // Alt+Enter 与 Enter 同义（统一消息队列，ADR-0014）
        (KeyCode::Enter, _) => Key::Enter,
        (KeyCode::Backspace, _) => Key::Backspace,
        (KeyCode::Left, _) => Key::Left,
        (KeyCode::Right, _) => Key::Right,
        (KeyCode::Home, _) => Key::Home,
        (KeyCode::End, _) => Key::End,
        (KeyCode::Up, _) => Key::Up,
        (KeyCode::Down, _) => Key::Down,
        (KeyCode::PageUp, _) => Key::PageUp,
        (KeyCode::PageDown, _) => Key::PageDown,
        (KeyCode::Char(c), KeyModifiers::ALT) => Key::Alt(c),
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => Key::Char(c),
        _ => return None,
    })
}

/// 执行 [`App::press`] 返回的语义效果：runner job、session 库、
/// skill resolver、图片加载等外部资源在此接线。
pub(super) async fn execute_effect(
    app: &mut App,
    driver: &mut Driver,
    terminal: &mut TuiTerminal,
    effect: Effect,
) {
    match effect {
        Effect::Prompt { text, images } => submit_prompt(app, driver, &text, images),
        Effect::Compact(instructions) => {
            if driver
                .runner
                .submit(SessionJob::Compact { instructions })
                .is_err()
            {
                app.finish_run(Some("内部错误：agent 任务已退出，无法压缩。".to_string()));
            }
        }
        Effect::Continue => {
            if driver.runner.submit(SessionJob::Continue).is_err() {
                app.finish_run(Some("内部错误：agent 任务已退出，无法续跑。".to_string()));
            }
        }
        Effect::Cancel => {
            // 取消在途 job（排队 job 保留）；提问弹层随中断关闭，回答
            // 通道丢弃（工具侧经取消令牌/通道关闭解除阻塞，不挂起）
            driver.runner.cancel_current();
            driver.pending_question = None;
        }
        // INSERT `Ctrl+G`：挂起 TUI 运行外部编辑器（ADR-0017），退出后写回
        Effect::OpenEditor => edit_input_in_editor(app, terminal).await,
        Effect::ListSessions => effects::list_sessions(app, &driver.session).await,
        Effect::Resume(id) => {
            effects::resume_session(app, &mut driver.session, &driver.handle, id).await;
        }
        Effect::ListTree => effects::list_tree(app, &driver.session).await,
        Effect::BranchTo(entry_id) => {
            effects::branch_to(app, &mut driver.session, &driver.handle, entry_id).await;
        }
        Effect::ListModels => effects::list_models(app, &driver.model),
        Effect::SwitchModel(id) => {
            effects::select_model(app, &mut driver.model, &driver.handle, &driver.session, &id);
        }
        Effect::SetReasoning(level) => {
            effects::set_reasoning(
                app,
                &mut driver.model,
                &driver.handle,
                &driver.session,
                &level,
            );
        }
        Effect::CancelModelSwitch => effects::cancel_model_switch(app, &mut driver.model),
        Effect::ListSkills => {
            // 列出时顺带刷新补全快照：会话期间新增的 skill 也能被 Tab 补全
            let catalog = driver.skill_resolver.catalog();
            app.show_skills(
                catalog
                    .into_iter()
                    .map(|skill| SkillEntry {
                        name: skill.name,
                        description: skill.document.description,
                        scope: skill.scope,
                    })
                    .collect(),
            );
        }
        Effect::LoadSkill(invocation) => {
            match driver.skill_resolver.activate(&invocation.name) {
                // 注入消息经事件管线回流：聊天区压缩展示 + session 落库自动
                // 生效；fire-and-forget，邮箱 FIFO 保证先于紧随的 job 生效
                Ok(skill) => {
                    let _ = driver
                        .handle
                        .inject_user_message(&super::app::skill_load_message(
                            &skill,
                            invocation.args.as_deref(),
                        ));
                }
                Err(error) => app.warn(format!("载入 skill {:?} 失败：{error}", invocation.name)),
            }
        }
        Effect::AttachImage(path) => effects::attach_image(app, &std::path::PathBuf::from(path)),
        Effect::CopyText(text) => effects::copy_to_clipboard(app, text).await,
        Effect::SubmitQuestionAnswer(answer) => {
            // 作答回传：发送端即返回（工具侧在 agent 任务内 await，
            // 失败仅可能因 agent 任务退出，无需提示）
            if let Some(answer_tx) = driver.pending_question.take() {
                let _ = answer_tx.send(answer);
            }
        }
        Effect::CancelQuestion => {
            // 丢弃回答通道：工具侧收到关闭转为错误结果回喂模型
            driver.pending_question = None;
        }
        Effect::NewSession => {
            effects::new_session(app, &mut driver.session, &driver.handle).await;
        }
    }
}

/// `Effect::Prompt` 的实现：用户主动提交 prompt（重置 goal 计数、丢弃
/// 上一轮残留的在途问题回答通道、展开 mention、提交 runner job）。
fn submit_prompt(
    app: &mut App,
    driver: &mut Driver,
    text: &str,
    images: Vec<nomic_ai::ImageContent>,
) {
    // 用户主动提交：重置 goal 模式连续追问计数；丢弃上一轮残留的
    // 在途问题回答通道（防御：正常路径弹层已随运行结束关闭）
    driver.goal.reset();
    driver.pending_question = None;
    // 发送前展开有效 `@skill:` / `@file:` mention；无效标记原样保留
    // （`@file:` 相对路径以当前 session 的 workspace 为基准）
    let text = mention::expand_mentions(text, &driver.skill_resolver, &driver.session.base_dir());
    if driver
        .runner
        .submit(SessionJob::Prompt { text, images })
        .is_err()
    {
        // runner 已退出：不会有回执，立即回到空闲态并提示
        app.finish_run(Some("内部错误：agent 任务已退出，消息未发送。".to_string()));
    }
}

/// 测试辅助：最小 agent actor 句柄（provider 不会被调用——仅承载
/// fire-and-forget 变更与查询的邮箱语义）。
#[cfg(test)]
pub(in crate::tui) fn dummy_handle() -> AgentHandle {
    struct NoopProvider;

    impl nomic_ai::Provider for NoopProvider {
        fn stream(
            &self,
            _model: &Model,
            _context: &nomic_ai::Context,
            _options: &nomic_ai::StreamOptions,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> nomic_ai::AssistantStream {
            unimplemented!("测试不发起请求")
        }
    }

    let (agent, _events) = Agent::builder()
        .model(Model {
            id: "test-model".to_string(),
            name: "test-model".to_string(),
            api: nomic_ai::ApiKind::OpenAiCompletions,
            provider: "test".to_string(),
            base_url: "http://localhost".to_string(),
            reasoning: false,
            context_window: 128_000,
            max_tokens: 4_096,
            cost_input: 0.0,
            cost_output: 0.0,
            cost_cache_read: 0.0,
            cost_cache_write: 0.0,
        })
        .provider(std::sync::Arc::new(NoopProvider))
        .system_prompt("test")
        .build();
    let (handle, _task) = agent.spawn();
    handle
}
