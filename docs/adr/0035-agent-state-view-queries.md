# ADR-0035: agent 查询改读共享状态视图（修订 ADR-0022 的邮箱查询）

- 状态：已接受
- 日期：2026-08-21
- 修订：ADR-0022（agent actor 模型）中「查询同样走邮箱 oneshot（严格
  actor，不引入共享只读快照）」一条
- 关联：ADR-0030（web UI 的快照 + 事件增量模型）

## 背景

ADR-0022 把 agent 的全部交互（含 `messages` / `model` / `reasoning` /
`context_tokens` / `stats` 查询）收进 actor 命令邮箱，严格串行。当时
唯一的交互端是 TUI，渲染完全由事件流驱动，查询只在非运行状态使用，
「查询排队到在途命令之后」不构成问题。

web UI（ADR-0030）引入了会话快照语义：切换查看一个 session 时前端先经
`get_state` 拉全量快照，再由事件流增量驱动。而 run 类命令
（prompt / compact / continue）把整轮 agent loop 包进一条邮箱命令，运行
期间（数秒到数分钟）邮箱不再被消费——对一个正在运行的 session 拉快照，
查询会排队到 run 结束，前端 30 秒请求超时触发，`loadSession` 抛错、
`sessionId` 不更新：表现为「从活跃会话切走后无法切回，提示 get_state
超时」。supervisor 的 `status` / `list` 查询子 agent 消息数也有同样的
潜在挂起。

## 决策

引入 agent 状态的共享只读视图（`agent/state.rs` 的 `StateView`，经
`Arc<RwLock<…>>` 共享），修订 ADR-0022 的查询路径：

- **单写者**：视图由 agent 本体在状态变更点同步维护——消息落史（与
  `MessageEnd` 事件同一点，复用其上下文 token 估算）增量推入；历史整体
  替换 / 弹出（压缩、restore、清空、continue 弹出失败尾）全量重同步；
  模型 / 思考级别 / 统计变更只同步元信息。锁内只有短临界区，跨 await
  不持锁。
- **查询读视图**：`AgentHandle` 的五个查询方法改为同步方法，直接读
  视图、不经邮箱——运行中即时返回「最后一次应用的状态」。actor 任务
  panic（邮箱关闭）后查询仍报告 `ActorError::Gone`，检测口径不变。
- **快照隔离的代价**：查询不再保证读到仍在邮箱排队的 fire-and-forget
  变更（原邮箱 FIFO 的「可见即生效」）。需要读己之写时经新增的
  `AgentHandle::flush()` 屏障（一条带 oneshot 回执的空命令）同步后再
  查询。
- 变更命令与 run 命令的邮箱路径、FIFO 顺序与回执语义不变。

## 备选方案

- **web 层事件溯源镜像**（`forward_events` 用事件流重建消息历史）：
  压缩后的历史形状、`restore` / `clear` 等无事件变更都要在 web 层复刻
  core 内部语义，脆弱且双写漂移；stats 等字段根本不在事件流里。否决。
- **turn 边界服务邮箱查询**（run 期间在 turn 边界抽干查询命令）：
  需要 agent loop 内回调 actor 的 seam，侵入 loop 结构，且最坏延迟仍为
  一个 turn（长工具执行 / 长生成仍可能超时）。否决。

## 后果

- `get_state` 对运行中 session 即时返回：消息到最近一次落史为止，
  切换查看后由事件流增量补齐在途部分（前端合并逻辑对缺失的
  `MessageStart` 已有兜底）。
- 查询方法签名由 async 改为 sync（内部 project，调用点已同步适配）；
  新增 `flush()` 屏障 API。
- 视图维护与消息落史共用同一代码点，不存在两份状态漂移；直接驱动
  `Agent`（不经 actor）时视图照常维护、无人读取，代价为每条落史消息
  一次克隆。
