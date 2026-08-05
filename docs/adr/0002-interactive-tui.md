# ADR-0002: 交互 TUI — ratatui 最小可交互版

## Status

Accepted

## Date

2026-07-26

## Context

ADR-0001 把交互 TUI 列为后续里程碑；M1（agent loop + 四工具 + print 模式）与
session 持久化（SQLite、resume）已落地。现在实现交互模式：无 `-p` 时进入 TUI，
支持多轮流式对话。

已与需求方确认的决策：

- 技术栈：**ratatui + crossterm**（Rust 生态事实标准；ADR-0001「Rust 生态有更自然
  表达的，采用 Rust 风格实现」的延续。pi-tui 的零依赖差量渲染在 Rust 中无对应必要）。
- 范围：**最小可交互版** —— 流式对话渲染、工具执行状态展示、输入框、Esc 取消、
  历史滚动、session 落库与 resume。
- 位置：**nomic-cli 内新增 `tui` 模块**，不独立 crate（当前只有一个消费方，
  独立抽象的复用收益为零）。

## Decision

### 入口与模式分发

`main.rs` 只做 CLI 解析与分发：有 `-p/--print` → `print` 模块（现有逻辑原样搬出，
行为不变）；否则 → `tui::run`。provider/model 解析、stream options、session 初始化
（新建 / `--continue` / `--session`）抽为共享函数，两种模式复用。

### 模块划分（crates/nomic-cli/src/tui/）

- `app.rs`：纯状态层，不碰终端。
  - `ChatItem`：`User(String)` / `Assistant { blocks, done }`（blocks 为
    `Text | Thinking` 有序块）/ `ToolExec { name, args, status }`。
  - 按 `(index, delta)` 累积流式 assistant 内容（ADR-0001 既定的消费方义务）；
    `ToolExecutionStart/Update/End` 更新工具项状态。
  - 输入缓冲（单行 + 光标）、历史滚动偏移、运行状态机（Idle / Running）。
  - 全部逻辑可脱离终端单测。
- `ui.rs`：纯渲染 —— 从 `&App` 构建 `Vec<Line>`（用户消息、assistant 文本、
  thinking 暗色斜体、工具行带 ✓/✗/▶ 状态），`Paragraph + Wrap` 绘制历史区；
  底部输入框 + 状态栏（模型、运行中提示、session id）。
- `mod.rs`：`run()` —— 终端初始化/恢复（raw mode + alternate screen，guard Drop
  保证恢复）、事件循环（`tokio::select!` 于 crossterm `EventStream` 与 agent 事件
  channel）、agent driver 任务。

### Agent driver 任务模型

`Agent::prompt(&mut self)` 要求独占可变引用，且要在多轮间复用（持有消息历史），
因此 agent 由**专属 tokio 任务**持有：TUI 经 mpsc 发送 prompt（附本轮
`CancellationToken`），driver 运行 loop 并经既有事件 channel 回传 `AgentEvent`，
完成后回 `Idle` 信号。Esc → 取消本轮 token；Ctrl+C → 运行中先取消、空闲时退出。

运行中新提交的处理：prompt 与模板调用**拒绝**（状态栏提示「等待当前运行结束」），
不实现 pi 的 steering/follow-up 队列（ADR-0001 已将其裁出 M1，此处同理）。
slash 命令按是否触碰 agent/driver 状态分流：**本地命令**（`/help`、`/copy`、
`/skill` 列表、`/image`、`/quit`）运行中照常执行——长时间运行的工具调用
不应阻塞它们；**会话命令**（`/new`、`/resume`、`/tree`、`/compact`、`/retry`、
`/models`、`/skill:<name>`）要经 driver 串行修改 agent 上下文，仍须等本轮结束。

### Session 与 resume

复用 print 模式的事件驱动落库：事件循环中 `MessageEnd` 定稿点调
`append_message`，失败仅告警。`--continue`/`--session` 恢复的历史消息直接渲染进
聊天区，新消息续写同一 session。

### 键位（最小集）

- `Enter` 提交；`Esc` 取消当前运行；`Ctrl+C` 取消运行/空闲退出；`Ctrl+D` 空闲退出。
- `↑/↓` 单行输入无历史导航，用于滚动聊天区；`PgUp/PgDn` 翻页；鼠标滚轮滚动。
- 输入为单行编辑（字符、退格、←/→ 光标、Home/End）。多行输入留给后续迭代。

## Non-goals

- slash 命令、`/resume` 会话树浏览、thinking 折叠交互、diff 高亮、autocomplete。
- 多行编辑器（tui-textarea 等）。
- markdown 渲染（assistant 文本按纯文本软换行展示）。
- steering/follow-up 消息队列。
- nomic-tui 独立 crate。

## Consequences

- workspace 新增 `ratatui`、`crossterm`（`event-stream` feature）两个依赖，
  仅 nomic-cli 使用。
- `main.rs` 重构为分发器；print 模式行为不变（代码搬移，无逻辑改动）。
- app 状态层脱离终端可测：delta 累积、工具状态迁移、滚动边界均有单测；
  ui 渲染用 ratatui `TestBackend` 做快照式冒烟测试。
- 后续增强（slash 命令、多行输入、markdown 渲染）都可在 `tui/` 模块内演进，
  不影响 core。

## Amendments（现状修订，不改写历史决策）

### 2026-07-28：slash 命令与自动补全落地

「Non-goals」中的 slash 命令与 autocomplete 已实现，自此移出非目标：

- **命令集（最小版）**：`/help`（列出命令）、`/new`（清空 agent 上下文并新建
  session 续写落库）、`/quit`（别名 `/exit`）。命令注册表是补全候选与 `/help`
  输出的唯一来源。
- **补全交互**：输入以 `/` 开头（命令名阶段、光标在末尾）时在输入框上方弹出
  候选；`Tab` 接受选中项/循环，`↑/↓` 移动选中（此时让位于聊天滚动），`Enter`
  在未精确匹配时先填入候选，`Esc` 优先关闭弹层再取消运行。
- **支撑改动**：`nomic-core` 新增 `Agent::clear_messages()`；driver 任务的消息
  类型由 prompt 元组改为 `DriverJob { Prompt, Clear }` 枚举；`ChatItem` 新增
  `System` 变体承载本地命令输出（不进上下文、不落库）。
- `/resume` 会话树浏览、多行编辑器、markdown 渲染仍为非目标。
