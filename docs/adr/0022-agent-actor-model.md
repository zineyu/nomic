# ADR-0022: agent 使用方式改为 actor 模型（AgentHandle 命令邮箱）

## Status

Accepted

## Date

2026-08-14

## Context

`Agent`（nomic-core）是 `&mut self` 直接调用 API：`prompt` / `retry` /
`compact` 为异步 loop，`inject_user_message` / `clear_messages` /
`restore_messages` / `set_model` / `set_provider` / `set_reasoning` 为
同步变更。所有变更方法都靠 doc 注释约定「仅非运行状态（prompt 返回后）
调用」，纪律无编译期或运行时强制——在运行中误调即静默破坏上下文。

事实上 actor 模式已经存在，但散在 CLI 层：TUI 的 `driver.rs` 手写
专属 tokio 任务持有 `Agent`，`DriverJob` / `DriverDone` 枚举走 mpsc
通道串行执行；print 模式则 `tokio::spawn` 移动 `agent` 本体。两个调用
端各自拼装并发骨架，core 没有提供正式的使用方式，嵌入式用户只能照抄
driver 或裸持 `&mut Agent`。

## Decision

在 nomic-core 新增 actor 层，作为 agent 的推荐外部使用方式：

- **`Agent::spawn(self) -> (AgentHandle, JoinHandle<()>)`**：agent 本体
  移入专属 tokio 任务，任务内串行处理命令邮箱（`AgentCommand`，
  crate 私有）；事件流接收端仍在 builder `build()` 时取得，事件推送
  模型不变。
- **`AgentHandle`**（`Clone` + `Send`）：每个命令一个方法。`prompt` /
  `prompt_with_images` / `retry` / `compact` 携带本轮取消令牌，经
  oneshot 回执返回结果；`inject_user_message` / `clear_messages` /
  `restore_messages` / `set_model` / `set_provider` / `set_reasoning`
  为fire-and-forget（邮箱 FIFO 即顺序保证）；查询操作
  `messages` / `context_tokens` / `model` / `reasoning` 同样走邮箱
  oneshot（严格 actor，不引入共享只读快照）。
- **错误**：统一 `ActorError`——`Gone`（actor 任务已退出，发送失败或
  oneshot 被丢弃）与 `Loop(AgentError)` / `Compaction(CompactionError)`
  透传。
- **`steering_handle()`** 直接返回共享 `SteeringQueue` 克隆
  （ADR-0014：该句柄本就并发安全、可在运行中随时调用，不经邮箱）。
- 运行中调用纪律由结构保证：handle 方法均为 `&self`，任意时机可调，
  命令在 actor 内串行执行，原「仅非运行状态调用」约定对 handle
  调用方不复存在（`Agent` 直接 API 的约定不变）。

### 边界

- `Agent` 保持公开：core 现有 loop 测试与嵌入式场景仍可直接驱动
  loop（单任务内顺序 await 时直接 API 更轻）；actor 是推荐的并发
  使用方式，不强制唯一入口。
- nomic-cli 迁移：TUI driver 改为薄适配层（`DriverJob` → handle
  调用，`DriverDone` 回执与 goal 模式逻辑不变）；print 模式改用
  handle。
- 事件模型不改：`AgentEvent` 仍经 build 时取得的 unbounded 通道推送，
  actor 死亡（panic / 全部句柄断开退出）时事件通道随 agent 丢弃而
  关闭，调用端经既有 channel 关闭路径感知。

## Consequences

- 「何时可调」的注释纪律对 handle 调用方消失，误用面收敛；
  新调用端不再需要手写 driver 任务。
- 调用链多一跳邮箱转发（unbounded send + oneshot），开销可忽略；
  TUI 变为 事件循环 → driver 任务 → actor 任务 两跳，由 driver 层
  吸收，事件循环代码不变。
- actor panic 时所有挂起与后续调用得到 `ActorError::Gone`，事件通道
  关闭——与现状（driver 任务 panic → channel 关闭）同一检测口径。
