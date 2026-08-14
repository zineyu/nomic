//! agent driver：薄适配层——agent 本体由 core 的 actor 任务持有
//! （[`Agent::spawn`]，ADR-0022），driver 任务串行执行事件循环提交的
//! job（prompt / 压缩 / 重试 / 模型切换等）并回传结果；事件循环的
//! 唤醒处理（[`handle_wake`] / [`handle_prompt_done`] / [`next_wake`]）与
//! 按键映射（[`map_key`]）、Effect 外部资源接线（[`execute_effect`]）也在此。

use anyhow::{Context as _, Result};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures::StreamExt as _;
use nomic_ai::{Message, Model, StopReason, ThinkingLevel};
use nomic_core::{Agent, AgentEvent, Compaction};
use nomic_session::SessionRecorder;
use nomic_skills::SkillResolver;
use nomic_tools::TodoStore;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::app::{App, Effect, Key, SkillEntry};
use super::effects::{self, ModelSwitcher, SessionBinding};
use super::goal::{GoalNudger, Nudge};
use super::terminal::edit_input_in_editor;
use super::{TuiTerminal, panic_payload_text};
use crate::model::ModelResolver;

/// 提交给 agent driver 的任务。
pub(super) enum DriverJob {
    /// 运行一轮 prompt（附图片附件与本轮取消令牌）
    Prompt(String, Vec<nomic_ai::ImageContent>, CancellationToken),
    /// 手动压缩上下文（`/compact [聚焦指令]`，附本轮取消令牌）
    Compact(Option<String>, CancellationToken),
    /// 重试最近一轮失败的响应（`/retry`，附本轮取消令牌）
    Retry(CancellationToken),
    /// 向 agent 历史注入一条 user 消息（`/skill:<name>` 手动载入），不启动 run
    Inject(String),
    /// 清空 agent 上下文（`/new`）
    Clear,
    /// 整体替换 agent 上下文（`/resume` 恢复历史 session）
    Restore(Vec<Message>),
    /// 切换模型（`/models`；上下文保留，spec 已按启动同一口径解析；
    /// 跨 provider 时携带新连接实现）
    SwitchModel(ModelSwitch),
    /// 设置思考级别（模型切换流程第二步确认；None 关闭）
    SetReasoning(Option<ThinkingLevel>),
}

/// `/models` 模型切换载荷：跨 provider 时携带新连接实现与分层的 api_key。
pub(super) struct ModelSwitch {
    pub(super) model: Model,
    pub(super) provider: Option<ProviderSwitch>,
}

/// 跨 provider 切换的新连接：provider 实现 + 按启动同一口径分层
/// （环境变量 > `providers.<名字>.api_key` > 平铺配置；CLI 的 `--api-key`
/// 属于启动 provider，不参与运行时切换分层）的 api_key。
pub(super) struct ProviderSwitch {
    pub(super) provider: std::sync::Arc<dyn nomic_ai::Provider>,
    pub(super) api_key: Option<String>,
}

/// agent driver 完成的任务回执。
pub(super) enum DriverDone {
    /// 一轮 prompt 结束（Err 为 agent loop 错误）
    Prompt(Result<PromptEnd, String>),
    /// 一次手动压缩结束（Ok(None) 表示无可压缩内容；Err 为摘要失败）
    Compact(Result<Option<Compaction>, String>),
    /// 一次重试结束（Ok(false) 表示无可重试状态；Err 为 loop 错误）
    Retry(Result<bool, String>),
}

/// 一轮 prompt 的结束回执（goal 模式是否自动追问的判定依据）。
pub(super) struct PromptEnd {
    /// 是否正常结束：用户取消（Ctrl+C）或响应以 Error/Aborted
    /// 收尾时为 false——失败与中断的恢复由用户主导，不自动追问
    ended_normally: bool,
}
/// 启动 agent driver：agent 经 [`Agent::spawn`] 移入 core actor 任务，
/// driver 任务串行执行 job（经 [`nomic_core::AgentHandle`] 转发），完成后回传结果。
// 参数均为 driver 的独立组成部分，打包为参数结构只会增加间接层
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_driver(
    agent: Agent,
    recorder: Option<SessionRecorder>,
    models: ModelResolver,
    model: Model,
    skill_resolver: SkillResolver,
    reasoning: Option<ThinkingLevel>,
    todos: TodoStore,
) -> Result<(Driver, mpsc::UnboundedReceiver<DriverDone>)> {
    let (handle, actor_task) = agent.spawn();
    let (job_tx, mut job_rx) = mpsc::unbounded_channel::<DriverJob>();
    let (done_tx, done_rx) = mpsc::unbounded_channel::<DriverDone>();
    let driver_task = tokio::spawn(async move {
        while let Some(job) = job_rx.recv().await {
            match job {
                DriverJob::Prompt(text, images, cancel) => {
                    let result = handle
                        .prompt_with_images(&text, &images, cancel.clone())
                        .await;
                    let done = match result {
                        Ok(messages) => {
                            // goal 模式追问判定：用户取消（Ctrl+C）或
                            // 响应以 Error/Aborted 收尾时不算正常结束
                            let last_stop =
                                messages.iter().rev().find_map(|message| match message {
                                    Message::Assistant(assistant) => Some(assistant.stop_reason),
                                    _ => None,
                                });
                            let ended_normally = !cancel.is_cancelled()
                                && !matches!(
                                    last_stop,
                                    Some(StopReason::Error | StopReason::Aborted)
                                );
                            Ok(PromptEnd { ended_normally })
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    if done_tx.send(DriverDone::Prompt(done)).is_err() {
                        return;
                    }
                }
                DriverJob::Compact(instructions, cancel) => {
                    let result = handle
                        .compact(instructions.as_deref(), cancel)
                        .await
                        .map_err(|error| error.to_string());
                    if done_tx.send(DriverDone::Compact(result)).is_err() {
                        return;
                    }
                }
                DriverJob::Retry(cancel) => {
                    let result = handle
                        .retry(cancel)
                        .await
                        .map(|outcome| outcome.is_some())
                        .map_err(|error| error.to_string());
                    if done_tx.send(DriverDone::Retry(result)).is_err() {
                        return;
                    }
                }
                // 变更为 fire-and-forget：邮箱 FIFO 保证其先于紧随的
                // prompt 生效；Err 即 actor 已退出，事件 channel 随之关闭，
                // 经 driver_failed 路径上报
                DriverJob::Inject(text) => {
                    let _ = handle.inject_user_message(&text);
                }
                DriverJob::Clear => {
                    let _ = handle.clear_messages();
                }
                DriverJob::Restore(messages) => {
                    let _ = handle.restore_messages(messages);
                }
                DriverJob::SwitchModel(switch) => {
                    // 先换 provider 再换模型：命令按序入邮箱，紧随的请求
                    // 一定跑在新 provider 的新模型上
                    if let Some(provider) = switch.provider {
                        let _ = handle.set_provider(provider.provider, provider.api_key);
                    }
                    let _ = handle.set_model(switch.model);
                }
                DriverJob::SetReasoning(level) => {
                    let _ = handle.set_reasoning(level);
                }
            }
        }
    });
    let driver = Driver {
        job_tx,
        current_cancel: None,
        task: Some(actor_task),
        adapter_task: Some(driver_task),
        alive: true,
        session: SessionBinding::new(recorder, std::env::current_dir().context("get cwd")?),
        model: ModelSwitcher::new(models, model, reasoning),
        skill_resolver,
        goal: GoalNudger::new(todos),
    };
    Ok((driver, done_rx))
}
/// 事件循环持有的驱动端资源。字段全私有（ADR-0024）：业务状态按关注点
/// 收在子结构（[`SessionBinding`] / [`ModelSwitcher`] / [`GoalNudger`]），
/// effects 函数经 [`execute_effect`] 转发拿到对应子结构，字段表不再是 interface。
pub(super) struct Driver {
    /// driver job 邮箱（提交 prompt / 压缩 / 恢复上下文 / 模型切换等任务）
    job_tx: mpsc::UnboundedSender<DriverJob>,
    /// 当前轮的取消令牌（Ctrl+C 取消用）
    current_cancel: Option<CancellationToken>,
    /// agent actor 任务的 JoinHandle：任务退出时取出详情转为 TUI 内错误提示
    task: Option<tokio::task::JoinHandle<()>>,
    /// driver 适配任务的 JoinHandle（与 actor 任务任一退出即整体不可用，
    /// [`driver_failed`] 等待先结束的一方并中止另一方）
    adapter_task: Option<tokio::task::JoinHandle<()>>,
    /// actor 是否存活；退出后其 channel 已关闭，事件循环跳过对应分支
    alive: bool,
    /// 会话落库绑定（recorder + cwd）：定稿点落库与父指针推进收在
    /// [`SessionRecorder`]（print 同一实现），`/tree` 分支与 `/new` /
    /// `/resume` 的换绑收在 effects::session
    session: SessionBinding,
    /// 两步模型切换状态机（`/models`）：当前模型/思考级别、待切换模型
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
    /// driver 任务完成（prompt 或手动压缩）
    AgentDone(DriverDone),
    /// spinner 帧推进
    Tick,
    /// 仅需重绘（其他鼠标事件）
    Redraw,
    /// agent driver 任务意外退出（panic 或提前返回），附详情
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
        Wake::AgentDone(done) => match done {
            DriverDone::Prompt(result) => {
                driver.current_cancel = None;
                handle_prompt_done(app, driver, terminal, result).await;
            }
            DriverDone::Compact(result) => {
                driver.current_cancel = None;
                let notice = match result {
                    // 压缩成功经 CompactionEnd 事件渲染与落库，这里无需重复处理
                    Ok(Some(_)) => None,
                    Ok(None) => Some("上下文很短，没有可压缩的内容。".to_string()),
                    Err(error) => Some(format!("压缩失败，上下文保持不变：{error}")),
                };
                app.finish_run(notice);
            }
            DriverDone::Retry(result) => {
                driver.current_cancel = None;
                let notice = match result {
                    // 重试成功经事件流渲染与落库，这里无需重复处理
                    Ok(true) => None,
                    Ok(false) => Some("最近一轮没有失败的响应，无需重试。".to_string()),
                    Err(error) => Some(format!("重试失败：{error}")),
                };
                app.finish_run(notice);
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
    result: Result<PromptEnd, String>,
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
        if end.ended_normally {
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
    match driver.goal.next(end.ended_normally && app.goal_mode()) {
        Nudge::Quiet => app.finish_run(None),
        Nudge::Capped(notice) => app.finish_run(Some(notice)),
        Nudge::Remind(reminder) => {
            let token = CancellationToken::new();
            if driver
                .job_tx
                .send(DriverJob::Prompt(reminder, Vec::new(), token.clone()))
                .is_ok()
            {
                driver.current_cancel = Some(token);
                app.begin_run();
            } else {
                app.finish_run(Some(
                    "内部错误：agent 任务已退出，goal 追问未发送。".to_string(),
                ));
            }
        }
    }
}
/// 等待下一个唤醒源：按键 / 鼠标 / agent 事件 / 本轮完成 / spinner 帧。
///
/// agent 侧 channel 与 driver 任务同生命周期：channel 关闭即任务退出
/// （job 发送端不会先于任务丢弃），统一转为 [`Wake::DriverFailed`]。
pub(super) async fn next_wake(
    app: &App,
    driver: &mut Driver,
    term_events: &mut EventStream,
    spinner_ticker: &mut tokio::time::Interval,
    events: &mut mpsc::UnboundedReceiver<AgentEvent>,
    done_rx: &mut mpsc::UnboundedReceiver<DriverDone>,
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
        maybe_done = async {
            if driver_alive {
                done_rx.recv().await
            } else {
                std::future::pending().await
            }
        } => match maybe_done {
            Some(done) => Wake::AgentDone(done),
            None => driver_failed(driver).await,
        },
    }
}
/// actor / 适配任务退出：任一退出即整体不可用（events 与 done channel
/// 的关闭都路由到这里）；等待先结束的一方取出详情（panic 负载等），
/// 中止另一方，转为 TUI 内提示。
pub(super) async fn driver_failed(driver: &mut Driver) -> Wake {
    driver.alive = false;
    let actor = driver.task.take();
    let adapter = driver.adapter_task.take();
    let detail = match (actor, adapter) {
        (Some(mut actor), Some(mut adapter)) => {
            tokio::select! {
                result = &mut actor => {
                    adapter.abort();
                    task_exit_detail("agent actor", result)
                }
                result = &mut adapter => {
                    actor.abort();
                    task_exit_detail("driver", result)
                }
            }
        }
        // 已报告过一次（events 与 done 两个 channel 先后关闭）
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
/// 执行 [`App::press`] 返回的语义效果：driver job、取消令牌、session 库、
/// skill resolver、图片加载等外部资源在此接线。
pub(super) async fn execute_effect(
    app: &mut App,
    driver: &mut Driver,
    terminal: &mut TuiTerminal,
    effect: Effect,
) {
    match effect {
        Effect::Prompt { text, images } => {
            // 用户主动提交：重置 goal 模式连续追问计数
            driver.goal.reset();
            let token = CancellationToken::new();
            if driver
                .job_tx
                .send(DriverJob::Prompt(text, images, token.clone()))
                .is_ok()
            {
                driver.current_cancel = Some(token);
            } else {
                // driver 已退出：不会有回执，立即回到空闲态并提示
                app.finish_run(Some("内部错误：agent 任务已退出，消息未发送。".to_string()));
            }
        }
        Effect::Compact(instructions) => {
            let token = CancellationToken::new();
            if driver
                .job_tx
                .send(DriverJob::Compact(instructions, token.clone()))
                .is_ok()
            {
                driver.current_cancel = Some(token);
            } else {
                app.finish_run(Some("内部错误：agent 任务已退出，无法压缩。".to_string()));
            }
        }
        Effect::Retry => {
            let token = CancellationToken::new();
            if driver.job_tx.send(DriverJob::Retry(token.clone())).is_ok() {
                driver.current_cancel = Some(token);
            } else {
                app.finish_run(Some("内部错误：agent 任务已退出，无法重试。".to_string()));
            }
        }
        Effect::Cancel => {
            if let Some(token) = &driver.current_cancel {
                token.cancel();
            }
        }
        // INSERT `Ctrl+G`：挂起 TUI 运行外部编辑器（ADR-0017），退出后写回
        Effect::OpenEditor => edit_input_in_editor(app, terminal).await,
        Effect::ListSessions => effects::list_sessions(app, &driver.session).await,
        Effect::Resume(id) => {
            effects::resume_session(app, &mut driver.session, &driver.job_tx, id).await;
        }
        Effect::ListTree => effects::list_tree(app, &driver.session).await,
        Effect::BranchTo(entry_id) => {
            effects::branch_to(app, &mut driver.session, &driver.job_tx, entry_id).await;
        }
        Effect::ListModels => effects::list_models(app, &driver.model),
        Effect::SwitchModel(id) => {
            effects::select_model(app, &mut driver.model, &driver.job_tx, &driver.session, &id);
        }
        Effect::SetReasoning(level) => {
            effects::set_reasoning(
                app,
                &mut driver.model,
                &driver.job_tx,
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
                Ok(skill) => {
                    // 注入消息经事件管线回流：聊天区压缩展示 + session 落库自动生效
                    let _ = driver
                        .job_tx
                        .send(DriverJob::Inject(super::app::skill_load_message(
                            &skill,
                            invocation.args.as_deref(),
                        )));
                }
                Err(error) => app.warn(format!("载入 skill {:?} 失败：{error}", invocation.name)),
            }
        }
        Effect::AttachImage(path) => effects::attach_image(app, &std::path::PathBuf::from(path)),
        Effect::CopyText(text) => effects::copy_to_clipboard(app, text).await,
        Effect::NewSession => {
            effects::new_session(app, &mut driver.session, &driver.job_tx).await;
        }
    }
}
