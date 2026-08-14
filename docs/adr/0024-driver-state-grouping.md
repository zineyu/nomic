# ADR-0024: driver 状态按关注点下沉，模型切换收进显式状态机

## Status

Accepted

## Date

2026-08-14

## Context

ADR-0022 把 TUI 的 driver 定位为「薄适配层」：agent 本体移入 core
actor 任务，driver 只转发 job。但 `Driver` 结构体随后长成了公共白板——
14 个 `pub(super)` 字段（`recorder`、`cwd`、`skill_resolver`、`models`、
`model`、`reasoning`、`pending_model`、`todos`、`goal_nudges`……），
`effects/*` 直接读写字段，seam 名存实亡：字段表就是 interface。

最典型的是两步模型切换（`/models` 先选模型、推理模型再选思考级别）：

- 「待切换」状态（`pending_model`）是 driver 字段，暂存/取用/放弃散在
  `effects/model.rs` 的三个入口函数里；
- Esc 放弃语义、切换幂等判断（同模型不切换、级别未变不设置）与
  选择落库分散在 driver.rs、effects/model.rs、app（选择器模式）等处，
  理解一个概念要全程跳读；
- 流程级行为（先暂存、确认时先切换后设级别、Esc 丢弃）无法单测——
  入口函数直接读写 `Driver`，测试得起事件循环。

ADR-0023 把落库策略收进 `SessionRecorder` 后，`Driver` 上的 `recorder`
只剩换绑与转发，分组下沉的条件成熟。

## Decision

`Driver` 只保留转发与生命周期所需的字段（job 邮箱、本轮取消令牌、
actor/adapter 任务句柄、存活标记），其余按关注点分组，下沉为各自
module 持有并定义的状态结构：

- **模型切换**：`effects/model/switch.rs` 新增 `ModelSwitcher` 状态机，
  持有当前模型/思考级别/待切换模型与运行时解析器。三个流转入口
  （`select` 第一步、`confirm_level` 第二步、`cancel` Esc 放弃）集中
  全部不变量：切换幂等（同模型不产生切换）、级别幂等（未变不发
  设置任务）、job 顺序（级别设置经同一邮箱紧随 SwitchModel）、跨
  provider 新连接构造（api_key 分层与启动同口径）。入口返回结果
  枚举，选择器 UI 接线与选择落库留在 `effects/model/mod.rs`。
- **会话落库**：`recorder` + `cwd` 收进 `effects/session.rs` 的
  `SessionBinding`；事件分支的落库收敛为 `session.record(&event)`
  一行，session 切换（`/new` / `/resume` / `/tree` 分支）的 recorder
  换绑方法化。
- **goal 追问**：`todos` + `goal_nudges` 收进 `tui/goal.rs` 的
  `GoalNudger`（连同 `MAX_GOAL_NUDGES` 与追问提示词）；追问判定
  （能否追问、上限暂停、计数清零时机）是一个返回结果枚举的方法。

driver 回到纯转发：`execute_effect` 按 Effect 族把对应子结构传给
effects 函数，`handle_prompt_done` 消费 `GoalNudger` 的判定结果。

## Consequences

- locality：切换流程的状态与不变量集中于 `switch.rs`；Esc 放弃语义
  只有一个家（`ModelSwitcher::cancel`）。
- driver 字段不再是公共白板：effects 经子结构的方法操作状态，
  `Driver` 的 `pub(super)` 字段只剩 job 邮箱与两个子结构。
- 流程级单测不起事件循环：状态机方法只依赖 job 邮箱
  （`mpsc::unbounded_channel` 即可构造），测试直接断言状态转移与
  job 顺序。
- 行为微调（仅内部错误路径）：级别确认时若模型切换 job 发送失败
  （driver 已退出），现在告警一次即中止，不再继续尝试级别设置产生
  第二次告警。
