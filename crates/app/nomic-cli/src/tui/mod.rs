//! 交互 TUI（ratatui + crossterm，设计见 docs/adr/0002）。
//!
//! 结构：
//! - [`app`]：纯状态层——对外为语义操作（按键 [`app::Key`] → [`app::Effect`]、
//!   应用事件、滚动、会话/附件管理），脱离终端可测；内部按关注点拆为
//!   chat（条目 + delta 累积 + 滚动）、input（草稿 + 编辑 + 补全）、
//!   queue（统一消息队列与 QUEUE 模式）、picker、question（`ask_user_question`
//!   提问弹层）子模块，`App` 只做组合与模式路由；INSERT `Ctrl+G` 外部编辑器
//!   （ADR-0017）由 [`terminal::edit_input_in_editor`] 接线，状态层只消费写回结果
//! - [`ask`]：`ask_user_question` 的 TUI 交互端（ADR-0029）——工具侧
//!   [`ask::TuiQuestionSink`] 与事件循环间的提问通道与在途问题
//! - [`effects`]：Effect 执行逻辑，按族分组为子模块——`model`
//!   （模型 + 思考级别两步流）、`session`（resume / tree / branch /
//!   new 与 recorder 换绑；定稿点落库收在 `nomic_session::SessionRecorder`）、
//!   `clipboard`（粘贴 / 复制 / 图片暂存）
//! - [`chat_lines`]：聊天区行组装（条目 → 带 gutter 的行 + 各条目起始行），
//!   状态层几何（渲染前主动计算）与渲染上屏共用同一实现，行数精确一致
//! - [`widgets`]：纯渲染——组合根 [`widgets::draw`] 布局后由各区域自定义
//!   widget（聊天区 / 输入框 / 状态栏 / 弹层 / 覆盖层）渲染
//! - [`driver`]：agent driver 任务（专属 tokio 任务持有 `Agent`）与事件循环
//!   的唤醒处理（按键映射、`Effect` 转发执行）；goal 模式追问的状态与
//!   策略收在 [`goal`]（`GoalNudger`），driver 只消费判定结果
//! - [`goal`]：goal 模式自动追问（todo 清单共享句柄 + 连续追问计数、
//!   上限与清零时机、追问提示词）
//! - [`terminal`]：终端生命周期（raw mode / alternate screen / 键盘增强）、
//!   panic 恢复 hook 与外部编辑器接线
//! - 本文件：`run` 事件循环主循环
//!
//! agent 由专属 tokio 任务持有（`Agent::prompt` 需要 `&mut self` 且跨轮复用），
//! TUI 经 mpsc 发送 prompt（附本轮 `CancellationToken`），agent 事件经既有
//! channel 回流；定稿点落库由 `SessionRecorder` 消费事件流完成。
//!
//! 错误策略：可预期错误（agent loop 失败、压缩失败、落库失败等）就地转为
//! 状态栏/聊天区提示；意外错误（driver 任务 panic）经 JoinHandle 捕获后在
//! 聊天区提示，TUI 保持存活供查看记录，而非静默退出。

mod app;
mod ask;
mod chat_lines;
mod driver;
mod effects;
mod goal;
mod markdown;
mod mention;
mod steering;
mod terminal;
mod theme;
mod widgets;

#[cfg(test)]
mod tests;

use std::io;

use anyhow::{Context as _, Result};
use crossterm::event::EventStream;
use nomic_core::Agent;
use nomic_session::SessionRecorder;
use nomic_tools::{QuestionSink, TodoStore};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

use app::{App, SkillEntry};
use ask::{PendingQuestion, TuiQuestionSink};
use driver::{handle_wake, next_wake, spawn_driver};
use terminal::{TerminalGuard, block_cursor, set_cursor_style};

use crate::{Cli, bootstrap};

type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// 运行交互 TUI。
#[allow(clippy::too_many_lines)]
pub async fn run(cli: &Cli) -> Result<()> {
    let boot = bootstrap::bootstrap(cli, bootstrap::SessionPolicy::Init).await?;

    let mut app = App::new(
        boot.model.name.clone(),
        boot.session.as_ref().map(|(_, id)| id.clone()),
        boot.model.context_window,
    );
    app.load_history(&boot.history);
    // `--image` 附件在 TUI 模式同样生效：作为首轮消息的暂存附件
    effects::stage_cli_images(&mut app, &cli.image);
    // 工具基准（workspace 严格归属）：工具、`@file:` 补全与 session 绑定
    // 共享同一句柄，resume/new 切换 session 时基准经句柄原地更新，
    // 下一次工具执行/补全即生效
    let base_dir = nomic_tools::BaseDir::new(Some(boot.workspace.clone()));
    let skill_resolver = boot.skill_resolver.clone();
    let skill_entries: Vec<SkillEntry> = skill_resolver
        .catalog()
        .into_iter()
        .map(|skill| SkillEntry {
            name: skill.name,
            description: skill.document.description,
            scope: skill.scope,
        })
        .collect();
    // 命令输入框 `/skill:` 补全与草稿 `@skill:` mention 补全共用同一快照
    app.command_mut()
        .set_available_skills(skill_entries.clone());
    app.input_mut().set_available_skills(skill_entries);
    // `@file:` 补全与工具共享同一基准：resume 切换 workspace 时自动跟随
    app.input_mut().set_mention_base(&base_dir);
    app.command_mut()
        .set_available_templates(boot.prompt_templates.clone());
    // 启动解析的思考级别（CLI 参数 / 配置文件）在进入 builder 前取出，
    // driver 据此维护 `models` 级别选择器的当前值
    let initial_reasoning = boot.stream_options.reasoning;

    let todo_store = TodoStore::new();
    // 提问通道（ADR-0029）：agent 任务内的 `ask_user_question` 工具经
    // 发送端把问题推入，事件循环在 `next_wake` 中接收并打开提问弹层
    let (question_tx, mut question_rx) = mpsc::unbounded_channel::<PendingQuestion>();
    let question_sink: std::sync::Arc<dyn QuestionSink> =
        std::sync::Arc::new(TuiQuestionSink::new(question_tx));
    let (agent, mut events) = Agent::builder()
        .model(boot.model.clone())
        .provider(boot.provider.clone())
        .system_prompt(boot.system_prompt)
        .tools({
            // 子 agent 可用的工具池（基础工具，不含管理工具本身；与主 agent
            // 共享同一基准句柄，随 session workspace 一并切换）
            let child_tools = nomic_tools::default_tools_with_skills_in_shared(
                &base_dir,
                boot.skill_resolver.clone(),
                todo_store.clone(),
                question_sink.clone(),
            );
            // supervisor 管理子 agent 生命周期
            let supervisor = std::sync::Arc::new(nomic_core::AgentSupervisor::new(
                boot.provider.clone(),
                boot.available_models,
                nomic_core::SupervisorConfig::default(),
            ));
            // 主 agent 工具 = 基础工具 + 多 agent 管理工具
            let mut tools = nomic_tools::default_tools_with_skills_in_shared(
                &base_dir,
                boot.skill_resolver,
                todo_store.clone(),
                question_sink,
            );
            tools.extend(nomic_tools::multi_agent::multi_agent_tools(
                supervisor,
                child_tools,
            ));
            tools
        })
        .messages(boot.history)
        .stream_options(boot.stream_options)
        .compaction(boot.compaction)
        // 统一消息队列（ADR-0014）：TUI 自持队列，实现 core 的注入点
        // （TurnInjection），运行中 Enter 直推入队，core 在 turn 边界经
        // 注入点弹出注入（不经 driver job 通道）
        .turn_injection(app.queue().handle())
        .build();

    // 落库器：恢复的 session 父指针从默认分支末端起算（分支场景下保证续写
    // 落在默认分支而非全局最新 entry）；读取失败退回自动链最新
    let recorder = match boot.session {
        Some((store, id)) => match store.latest_entry_id(&id).await {
            Ok(tip) => Some(SessionRecorder::with_tip(store, id, tip)),
            Err(error) => {
                app.warn(format!("读取分支末端失败，落库将链到最新 entry：{error}"));
                Some(SessionRecorder::new(store, id))
            }
        },
        None => None,
    };

    let _guard = TerminalGuard::enter().context("初始化终端失败")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("创建终端后端失败")?;

    let (mut driver, mut done_rx) = spawn_driver(
        agent,
        recorder,
        base_dir,
        boot.models,
        boot.model,
        skill_resolver,
        initial_reasoning,
        todo_store,
    );
    let mut term_events = EventStream::new();
    // spinner 帧推进：仅运行中需要动画，空闲时分支挂起不唤醒事件循环
    let mut spinner_ticker = tokio::time::interval(std::time::Duration::from_millis(100));
    // 光标形状随交互模式切换（vim 情境信号）：浏览态实心块，可键入态竖条
    let mut last_block_cursor = block_cursor(&app);
    set_cursor_style(last_block_cursor);
    loop {
        terminal
            .draw(|frame| widgets::draw(frame, &mut app))
            .context("绘制失败")?;
        let wake = next_wake(
            &app,
            &mut driver,
            &mut term_events,
            &mut spinner_ticker,
            &mut events,
            &mut done_rx,
            &mut question_rx,
        )
        .await;
        if handle_wake(wake, &mut app, &mut driver, &mut terminal).await || app.should_quit() {
            break;
        }
        // QUEUE 的就地编辑子状态不改变模式字段，同样触发形状切换
        let block = block_cursor(&app);
        if block != last_block_cursor {
            last_block_cursor = block;
            set_cursor_style(block);
        }
    }
    Ok(())
}

/// 提取 panic 负载文本（`panic!("...")` 的 `&str`/`String`），无法识别时给兜底描述。
fn panic_payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_string()
    } else {
        "未知负载".to_string()
    }
}
