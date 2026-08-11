# ADR-0014: 统一消息队列（steering 与 follow-up 合并）

## Status

Accepted

## Date

2026-08-07

## Context

ADR-0012/0013 落地了双队列：运行中 **Enter = steering**（turn 边界注入
本轮运行）、**Alt+Enter = follow-up**（本轮正常结束后 FIFO 发送），
QUEUE 模式跨双队列统一编辑。实际使用中双队列的认知成本高于收益：

- 用户要预判「这条消息该中途注入还是等结束」，而多数排队意图只是
  「把这句话发给 agent」，两种投递时机的区分对非专家用户是负担；
- 键位映射（Enter/Alt+Enter 去向不同）依赖 kitty 键盘增强协议才能
  区分，协议不可用时 Alt+Enter 退化为 Enter，语义悄然翻转；
- 实现面：TUI 维护两份异构存储（core 共享句柄 + 本地 VecDeque），
  QUEUE 编辑需跨边界分发（换位不跨段、插槽继承归属、drain 双源
  优先级），渲染与标题需分列计数。

关键观察：steering 的「队列未清空时 run 不结束」语义在顺利路径下
**涵盖** follow-up——运行中入队的消息会在 turn 边界逐条注入续行，
agent 处理完才结束本轮；follow-up 独立存在的价值只剩「不打断当前
任务的思路，等它完全收尾再说」，而这一诉求可用 Esc→Q 编辑队列或
结束后再发覆盖。

需求：**将 steering 与 follow-up 合并为单一消息队列——运行中入队
一律走 steering 注入语义；run 异常结束（取消/失败）时队列保留，
恢复后按 FIFO 作为下一轮 prompt 发送。**

## Decision

### 语义：单一队列，两种消费时机

- 只保留一份队列：core 的共享 `SteeringQueue`（ADR-0013）。TUI 的
  本地 follow-up `VecDeque` 删除。
- **运行中入队**（Enter）：进入统一队列，core 在 turn 边界逐条注入
  （one-at-a-time 不变，队列未清空 run 不结束）。
- **run 正常结束**：队列已被 core 排空，无 drain。
- **run 异常结束 / QUEUE 编辑冻结后空闲恢复**：TUI 从同一队列弹出
  队首，作为下一轮 prompt 提交（ADR-0012 的暂停保留口径不变）。
- **Alt+Enter 与 Enter 完全同义**：键位映射层直接把 Alt+Enter 归并
  为 Enter，`Key::AltEnter` 变体删除。

### TUI 简化

- `Route` / `QueueKind` / `QueuedMessage` 删除；QUEUE 模式条目空间
  即队列本身，换位/插槽不再跨段分发，`in_steering` 边界判定删除。
- 渲染：队列条目不再区分「即将注入/等待结束」两色，游标条目
  `❯` 高亮不变；运行中标题从「N 条转向 · M 条排队」简化为
  「N 条排队」。
- QUEUE 模式进入即冻结注入（防游标漂移）、退出解冻恢复的机制不变。

### 代码落点

- `nomic-core/src/steering.rs`：模块文档改为统一队列语义（类型与
  行为不变）。
- `nomic-cli/src/tui/app.rs`：删除 follow-up 存储与双队列分发；
  `drain_queue` 单源弹出；`press_alt_enter` 删除。
- `nomic-cli/src/tui/mod.rs`：`map_key` 的 Alt+Enter 归并为 Enter。
- `nomic-cli/src/tui/ui.rs`：队列区单色渲染与标题简化。

## Non-goals

- 恢复「等本轮完全结束再发」的独立 follow-up 语义：如真实需求
  浮现，以「条目级投递时机标记」而非复活双队列的形式重新引入。
- core 公开类型改名（`SteeringQueue` → `MessageQueue` 之类）：语义
  已扩展但类型行为兼容，改名属破坏性 API 变更，留待下次大版本。
- `steeringMode: "all"`、Esc 中止恢复草稿：沿用 ADR-0013 的裁剪。

## Consequences

- 运行中 Alt+Enter 行为变化：从「结束后发送」翻转为「turn 边界
  注入」（与 Enter 一致）。依赖 kitty 协议的用户感知到 Alt+Enter
  不再排队等结束——这是刻意的简化。
- TUI 删除一条队列实现路径：QUEUE 编辑、drain、渲染、会话切换
  清空均为单源，ADR-0013 的跨边界不变量（换位不跨段、插槽继承
  归属）随之消失。
- core crate 行为与公开 API 不变，仅文档口径更新；agent 侧的
  turn 边界注入、冻结标志机制原样保留。
- ADR-0012 的 follow-up 队列实现与 ADR-0013 的双队列编辑被本
  ADR 取代；两者的 QUEUE 模式交互（oil.nvim 式编辑、暂停保留）
  继续有效。
