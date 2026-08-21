# ADR-0033: session runner——actor 之上的会话级串行 job 语义

## Status

Accepted

## Date

2026-08-21

## Context

ADR-0022 把 `Agent` 收进 actor（`AgentHandle` 命令邮箱），但交互端需要
的不止单命令串行：prompt / 压缩 / 续跑共用队列串行消费、每个 job 独立
取消令牌、compact/continue 空结果的就地通知、compact 不产生
`AgentStart`/`AgentEnd` 需合成运行生命周期。这层「session runner」语义
在两个交互端各手写了一遍：

- TUI `driver.rs`：`DriverJob`/`DriverDone` 枚举 + driver 任务串行执行，
  `current_cancel` 持有在途令牌，goal 追问判定（取消或 Error/Aborted
  收尾不算正常结束）也在其中；
- web `session.rs`：`RunGate`（Mutex 队列 + AtomicBool 运行标志）+
  runner 任务，注释自证「出队发现队列空但 submit 已入队」的丢单竞态
  需要锁 + 原子量规避；compact 的 RunStarted/RunFinished 合成、空结果
  通知与错误补发 RunFinished 各写一份。

两份实现语义逐条对应（连通知文案都相同），seam 却不在 core：第三个
交互端出现时只能再次照抄，且任何语义修正（如取消窗口、空结果口径）
都要双端同步。

## Decision

在 nomic-core 的 actor 之上新增 `runner` 模块，提供 `SessionRunner`：

- **`SessionJob`**：run 类 job 枚举（`Prompt` / `Compact` /
  `Continue`）。注入、清空、恢复、模型/级别切换等 fire-and-forget
  变更不是 job，不进队列——直接调 `AgentHandle`，邮箱 FIFO 保证其
  先于紧随的 job 生效。
- **串行消费**：内部单任务经 mpsc 队列按提交顺序执行 job。队列即
  channel，深度/运行标志（`queued_len` / `is_running`）仅服务状态
  快照，不参与调度——不存在 web RunGate 的「队列+标志」双状态竞态。
- **取消**：每个 job 开始执行时获得独立 `CancellationToken`；
  `cancel_current()` 只中断在途 job，排队 job 保留。提交时若无在途
  job，令牌预放入在途槽位，覆盖「已提交未开始」的取消窗口（对齐
  TUI 原 `current_cancel` 语义）。
- **结果翻译**：`RunnerEvent::Finished(JobOutcome)`——compact 的
  `Ok(None)` 译为 `CompactOutcome::NothingToCompact`、continue 的
  `Ok(None)` 译为 `ContinueOutcome::NothingToContinue`（通知文案
  `NOTHING_TO_COMPACT` / `NOTHING_TO_CONTINUE` 常量化，双端同一
  口径）；prompt 结果汇总 `PromptOutcome::ended_normally()`（未被
  取消且尾部不以 Error/Aborted 收尾）。
- **生命周期翻译**：runner 对全部 job 统一发射 `Started` →
  `Finished`（失败同样收尾 `Finished`）。prompt/continue 的 agent
  事件流自带 `AgentStart`/`AgentEnd`，compact 没有——需要运行生命
  周期的交互端以 runner 事件为准（至少对 compact）。

交互端保留各自的薄 adapter：TUI 的 goal 模式追问、消息队列
（ADR-0014）与落库接线留在 driver；web 的 broadcast 翻译
（`ServerEvent`）、提问表与快照收集留在 session 模块。

### 边界

- runner 不持有 agent 事件流（仍在 builder `build()` 时取得），也不
  做落库——落库是交互端经 `SessionRecorder` 的既有 seam（ADR-0023）。
- runner 事件与 agent 事件是两条通道，相互顺序不保证；web 继续从
  agent 事件推导 prompt 的 RunStarted/RunFinished，仅 compact 改用
  runner 事件合成。
- `Agent` / `AgentHandle` 直接 API 不变；runner 是「交互端驱动一个
  会话」的推荐封装，supervisor 等多 agent 场景不需要它。

## Consequences

- TUI 删除 `DriverJob`/`DriverDone`/driver 任务（fire-and-forget 变更
  直调 handle），web 删除 `RunGate`/`run_loop`；两端 runner 语义单
  源化，修正只需改一处。
- web 的丢单竞态防护（锁+原子量）被 channel 调度取代，快照标志退化为
  只读统计。
- 新增调用端只需映射 `RunnerEvent` 到自身通知模型。
- TUI 模型切换状态机改经 `AgentHandle` 直调，其单测从「断言邮箱
  payload」改为「断言 handle 查询结果」；跨 provider 切换的 provider
  接线正确性由 core 的 actor/loop 测试覆盖（`set_provider` 重定向
  后续请求已有集成测试）。
