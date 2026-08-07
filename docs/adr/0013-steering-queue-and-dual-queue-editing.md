# ADR-0013: steering 队列（pi 式运行中转向）与双队列 QUEUE 编辑

## Status

Accepted

## Date

2026-08-07

## Context

ADR-0012 落地了 pi 式 follow-up 队列（运行中 Enter 排队、本轮正常结束
后 FIFO 自动发送），并把 steering 队列（运行中插入转向消息）裁为后续
增量——它需要 core agent 的 turn 边界消息注入支持（ADR-0001 的 M1
裁剪项）。

follow-up 只覆盖「排队等结束」；编码 agent 的另一高频诉求是**运行中
转向**：看到 agent 走偏（改错文件、漏了约束）时立即补充指令，而不是
等整轮结束再纠偏。pi 的语义（pi 0.83 官方文档）：

- 运行中 **Enter = steering**：当前 assistant turn 的工具调用执行完
  后、下一次 LLM 调用前，作为 user 消息注入当前 run；
- **Alt+Enter = follow-up**：全部工作结束后发送；
- `steeringMode` 默认 `one-at-a-time`：每个完成的 turn 投递一条；
- steering 未清空时 run 不结束（模型无工具调用也会注入续行）。

需求：**实现 pi 式 steering 队列，运行中 Enter 改为 steering，Alt+Enter
承接原 follow-up 入队；steering 与 follow-up 双队列统一纳入 QUEUE
模式（ADR-0012 的 oil.nvim 式编辑）管理。**

## Decision

### core：steering 队列与 turn 边界注入（one-at-a-time）

- `Agent` 持有共享 steering 队列 `SteeringQueue`（`Arc<Mutex<VecDeque<
  SteeringMessage>>>` + 冻结标志），`SteeringMessage { text, images }`；
  经 `Agent::steering_handle()` 暴露可克隆句柄——交互端在 agent 运行
  期间直推入队（driver 串行 job 通道无法中转运行中消息）。
- `run_loop` 在 `TurnEnd` 之后、`tool_calls.is_empty()` 退出判断之前
  弹出**一条** steering 消息：作为 user 消息进入历史与本次新增（图片
  附件沿用 `prompt_with_images` 的内容块排序），发出 `MessageStart`/
  `MessageEnd` 事件（交互端渲染与 session 落库经既有管线自动生效），
  继续 loop。**steering 未清空时 run 不结束**：模型无工具调用同样注入
  续行，直至队列清空。
- Error/Aborted 收尾、`terminate` 工具终止、取消时不注入，队列保留。
- 注入对 `prompt` 与 `retry` 的 loop 同样生效（同一 `run_loop`）。

### TUI：键位与双队列语义

- **运行中 Enter → steering 队尾**（行为变更：ADR-0012 的 Enter 语义
  翻转，与 pi 默认映射一致）；**运行中 Alt+Enter → follow-up 队尾**
  （ADR-0012 的原 Enter 行为移键）。本地 slash 命令照常立即执行，会话
  命令仍拒绝；模板调用展开后按同一键位入对应队列。依赖 kitty 键盘
  增强协议区分 Alt+Enter（与 Shift+Enter 同一前提）。
- **空闲下 Alt+Enter 与 Enter 同义**（无运行可转向，直接提交）。
- **发送优先级**：异常暂停后恢复（空闲 Enter 空草稿、QUEUE 退出恢复、
  本轮正常结束 drain）时 steering 先于 follow-up；正常结束的 run 其
  steering 已被 loop 排空，实际只发生在暂停恢复路径。
- **会话切换清空双队列**：`/new`、`/resume`、`/tree` 分支切换（沿用
  ADR-0012 口径）。
- goal 模式判定不变：follow-up 优先于 goal 追问；steering 在 run 内
  注入，不与 goal 判定交互。

### 双队列 QUEUE 模式（oil.nvim 式编辑扩展）

- QUEUE 模式的条目空间 = steering 条目（前）+ follow-up 条目（后），
  统一游标导航（`j`/`k`/`gg`/`G`）、删除（`dd`/`x`）、换位（`J`/`K`，
  不跨队列边界）、就地编辑（`i`/`a`/`Enter`）与插槽（`o`/`O`，新槽位
  继承游标条目所在队列）。
- **进入 QUEUE 模式同时冻结 steering 注入**（队列句柄的冻结标志，
  core 在 turn 边界检查）：用户手持缓冲编辑时运行仍在推进，不冻结会
  导致游标下标被 core 弹走漂移；退出 QUEUE 即解冻恢复。
- 渲染：steering 条目 gutter `»` 用 accent 色（即将注入），follow-up
  条目 `»` 用暗色（等待本轮结束）；游标条目仍为 `❯`。输入框标题：
  运行中「N 条转向 · M 条排队（Esc→Q 编辑）」；空闲暂停「队列暂停
  N 条 · Enter 发送下一条」。

### 代码落点

- `nomic-core/src/agent.rs`：`SteeringMessage` / `SteeringQueue`、
  `steering_handle()`；`run_loop` 的 turn 边界注入。
- `nomic-core/src/builder.rs`：可选 `steering_queue`（默认新建）。
- `nomic-cli/src/tui/app.rs`：`steering: SteeringQueue` 字段；运行中
  Enter/Alt+Enter 分发；双队列统一条目视图与 QUEUE 模式编辑分发；
  冻结/解冻接线；会话切换清空。
- `nomic-cli/src/tui/mod.rs`：`map_key` 增加 Alt+Enter；其余不变
  （steering 入队不走 driver job/Effect）。
- `nomic-cli/src/tui/ui.rs`：双队列渲染与标题。

## Non-goals

- `steeringMode: "all"`（每个 turn 投递全部）：one-at-a-time 已覆盖
  转向诉求；需要时加配置项，属独立增量。
- Esc 中止后把排队消息恢复到草稿（pi 行为）：沿用 ADR-0012 的暂停
  保留口径。
- follow-up 与 steering 条目互相转换（改条目所在队列）。
- print 模式的 steering（无交互输入源）。

## Consequences

- 运行中 Enter 语义从「排入 follow-up」翻转为「steering 转向」：
  ADR-0012 的提示文案、测试与 `/help` 同步更新；习惯旧行为的用户
  改用 Alt+Enter。这是刻意的 pi 对齐。
- core crate 新增公开类型 `SteeringMessage` / `SteeringQueue` 与
  `Agent::steering_handle()`；driver job 协议不变（steering 经共享
  句柄直推）。
- steering 注入的 user 消息随事件管线渲染与落库，resume 时按历史
  回放——session 记录可见「当时插入了什么转向」。
- QUEUE 模式条目空间变为双队列拼接，编辑/导航按队列归属分发；
  换位不跨边界，插槽继承所在队列。
- steering 让 run 的结束时机受用户输入影响：一轮 prompt 的实际
  时长可能因持续转向而延长（与 pi 一致的预期行为）。
