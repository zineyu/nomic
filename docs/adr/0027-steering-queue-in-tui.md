# ADR-0027: steering 队列上移到 TUI，core 只保留注入点

- 状态：已接受
- 日期：2026-08-15
- 修正：ADR-0013/0014 中「core 持有共享 `SteeringQueue`」的落点

## 背景

ADR-0013 把 pi 式 steering 队列落在 `nomic-core`：`SteeringQueue`（`Arc<Mutex<
VecDeque<SteeringMessage>>>` + 冻结标志）与 `SteeringMessage` 是 core 的公开
类型，`Agent::run_loop` 在每个 turn 边界直接 `pop_front` 注入。ADR-0014 合并
follow-up 后沿用这一落点，TUI 的 `Queue` 只是对 core 队列的薄封装。

回头看这个落点是分层错位：队列的存储（入队/编辑/换位/清空）、QUEUE 模式的
就地编辑与冻结语义，全部是 TUI 的交互能力；core 作为纯 agent loop，真正需要
的只是「每个完成的 turn 边界能否拿到下一条要注入的 user 消息」这一个注入点。
print 模式（ADR-0013 的非目标）与嵌入式用法都不需要队列，却被迫接受了 core
对特定排队实现的耦合。

## 决策

**core 不再拥有队列，只暴露注入点：**

- 删除 `SteeringQueue` / `SteeringMessage` 公开类型；新增
  `TurnMessage { text, images }` 与 trait `TurnInjection`：
  `fn next_message(&self) -> Option<TurnMessage>`。
- `Agent::run_loop` 在 turn 边界询问注入源一次：返回 `Some` 则作为 user
  消息注入续行（图片块在前、文本块在后，与 prompt 附件同一口径），返回
  `None` 且无更多工具调用时 loop 按常规终止。one-at-a-time、冻结、队列
  编辑等语义全部由实现方保证，core 不关心。
- builder 的 `steering_queue(SteeringQueue)` 改为
  `turn_injection(Arc<dyn TurnInjection>)`；`Agent` / `AgentHandle` 的
  `steering_handle()` 删除。

**TUI 承接队列能力：**

- `SteeringQueue`（共享句柄：`Arc<Mutex<VecDeque<TurnMessage>>>` + 冻结
  标志）与 QUEUE 编辑移入 `nomic-cli/src/tui/steering.rs`，实现
  `TurnInjection`；冻结期 `next_message` 返回 `None`（run 可正常结束、
  队列保留），解冻后恢复弹出。
- `Queue`（QUEUE 模式状态）继续持有 `SteeringQueue` 并作为注入源经
  builder 的 `turn_injection` 传入；TUI 的入队/编辑/冻结/drain 逻辑不变。

## 后果

- core 公开 API 变化：`SteeringMessage` / `SteeringQueue` 删除，
  `TurnInjection` / `TurnMessage` 新增，`steering_handle()` 删除——属破坏性
  变更，仅 TUI 是唯一消费方（print 模式本就无 steering）。
- 冻结从「core 队列内建标志」变为「注入源实现的返回 `None`」：core loop 的
  契约简化为只认 `Some`/`None`，不再需要理解冻结。
- core 集成测试改用测试专用注入源驱动注入点，验证注入时机与续行契约；
  `SteeringQueue` 的单元测试随类型移入 TUI。
- ADR-0013/0014 的交互语义（运行中 Enter 入队、turn 边界注入、QUEUE 编辑、
  暂停保留）不变，仅队列的代码落点从 core 移到 TUI。
