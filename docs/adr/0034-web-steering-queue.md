# ADR-0034: web steering 队列与队列区编辑（QUEUE 模式的 web 版）

- 状态：已接受（前端载体由 ADR-0046 改为 Flutter GUI；本 ADR 的队列协议与
  服务端语义不受影响）
- 日期：2026-08-20
- 承接：ADR-0013/0014（steering 与统一消息队列语义）、ADR-0027（队列落点：
  core 只保留 `TurnInjection` 注入点）、ADR-0030（web UI；其非目标列出
  「队列的图形化编辑（TUI QUEUE 模式的 web 版）」，本 ADR 落地该项）

## 背景

web 模式此前没有 steering：`SessionFactory` 以 `turn_injection: None` 构建
agent，运行中提交的 prompt 进入 core `SessionRunner` 的串行 job 队列，在
当前轮整体结束后才作为 follow-up 执行。该队列是 runner 内部的 mpsc
channel，对外只有计数（快照的 `queued` 字段），前端只显示「已排队 N 条」，
看不到内容、无法编辑——与 TUI 的统一消息队列（ADR-0014）能力与语义都
不对齐。

需求：web UI 显示 steering 队列内容，并支持编辑（文本 / 删除 / 上下移），
投递语义对齐 TUI——运行中入队的消息在 turn 边界注入本轮运行。

## 决策

**web 侧自持统一消息队列**（`web::MessageQueue`，对齐 ADR-0027 的落点：
队列存储与编辑是交互端能力，core 不动）：

- 会话构建时创建 `MessageQueue`（`Arc<Mutex<VecDeque<Entry>>>`），作为
  `turn_injection` 注入源装入 agent recipe；core 在每个完成的 turn 边界
  弹出队首注入本轮（one-at-a-time、队列未清空 run 不结束，ADR-0014
  语义不变）。
- 运行中提交的普通文本 prompt 改走入队（不再进 runner job 队列）；
  斜杠命令（`/compact`、`/continue`）仍走 runner 串行 job 队列。
- **编辑按稳定 id 寻址**（条目 id 为 session 内单调递增序号）：web 的
  编辑操作来自远程客户端、与 turn 边界弹出天然并发，下标寻址会因弹出
  漂移误伤他条——这是 TUI 用「进入 QUEUE 模式冻结注入」解决的问题，
  id 寻址使 web 无需冻结机制。
- **队列存用户输入原文**，`@skill:` / `@file:` mention 在**投递时**
  （弹出队首）展开：注入与 drain 两条消费路径同一口径，展开内容以投递
  时刻为准；展示与编辑的对象始终是原文。
- **drain**：run 正常结束但队列仍非空（注入点查询后的滞后入队竞态窗口）
  时，runner 事件侧弹出队首作为下一轮 prompt 提交，串行消费使各轮
  drain 成链直至清空；run 异常结束（取消/失败）时队列暂停保留（ADR-0012
  暂停保留口径），由下轮正常结束后的 drain 恢复。
- **协议**：快照的 `queued: usize` 计数替换为 `queue` 条目数组（id +
  原文 + 附件数；图片内容不回传）；每次队列变更（入队/弹出/编辑/删除/
  换位）广播全量 `queue_changed` 事件；新增 `update_queue_entry` /
  `remove_queue_entry` / `move_queue_entry` 三个 fire-and-forget 命令
  （空文本保存即删除，oil.nvim 空行忽略语义，与 TUI 同一口径）。

**前端**：输入框上方新增队列区（QueuePanel）：条目展示原文与附件数，
行内提供上移/下移/编辑/删除；编辑为就地 textarea（Enter 保存、
Shift+Enter 换行、Esc 取消）。队列状态服务端权威（queue_changed 全量
替换 + 快照回填），本地只持有「正在编辑」子状态。

## 后果

- web 运行中 prompt 的投递时机变化：从「当前轮整体结束后按序续跑」
  翻转为「当前 turn 的工具调用执行完后注入本轮运行」（与 TUI Enter
  一致）；`prompt_ack.queued` 语义不变（仍表示「未立即运行」）。
- 快照协议变化：`queued: number` → `queue: QueueEntry[]`——web 协议
  为内嵌前端的私有协议，前后端同版本发布，无兼容负担。
- 取消当前轮后排队消息不再自动续跑（旧行为：runner 排队 job 在取消后
  立即执行下一条），改为暂停保留、由后续 drain 恢复——停止按钮的
  「队列保留」提示自此名副其实。
- 运行中提交的斜杠命令留在 runner job 队列（不可见也不可编辑）：命令
  不是转向内容，且需要「等本轮结束」的语义；如未来需要可见性，以
  条目级类型标记扩展队列视图。
- TUI 的 `SteeringQueue`（下标游标 + 冻结）与 web 的 `MessageQueue`
  （id 寻址 + 广播）是同一语义的两种交互端实现，按 ADR-0027 的分层
  各自演化，不收敛共享代码。
