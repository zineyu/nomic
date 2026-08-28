# ADR-0037: 目标驱动运行（goal 命令）与运行时工具集替换

- 状态：已接受
- 日期：2026-09-04

## 背景

原 goal 模式是开关式的：开启后 react loop 停止且 todo 清单未全部完成时，
自动以 user 消息列出未完成的 todo 追问模型。它有两个结构性弱点：

- **完成判定的权威是 todo 清单而非目标本身**：模型忘记写 todo、或清单
  与真实目标脱节时，追问判定失效（清单空了但目标没达成，或反之）。
- **目标从未显式交给模型**：开关不携带任何内容，模型不知道「要追到
  什么程度才算完」，只能从历史推断。

需求：`goal <目标>` 把目标显式交给模型——目标包装为提示词提交；agent
持续工作直到主动调用 `goal_done` 汇报完成；react loop 停止而 `goal_done`
未被调用时，以 user 消息**复述目标与要求**继续追问。目标驱动运行期间
agent 不应打断用户：换出 `ask_user_question`，换入 `goal_done`。

## 决策

### 完成判定：`goal_done` 工具 + 共享 `GoalSession`（nomic-tools）

- 新增 `goal_done` 工具（参数 `summary` 必填，强制模型汇报完成情况），
  执行时标记共享 [`GoalSession`]（`objective` + 完成标志，原子量）完成，
  结果带 `terminate: true`——目标汇报即终止本轮运行，不再空转收尾。
- `GoalSession` 由交互端创建（`goal <目标>` 时），与工具共享同一句柄：
  agent 任务内的工具执行与事件循环的追问判定经它通信，无需新事件通道。
- 工具集变换 `goal_tools(base, session)`：去掉 `ask_user_question`，加入
  绑定该会话的 `goal_done`。

### 运行时整体替换工具集（nomic-core）

工具集从创建期固定改为可运行时整体替换：`Agent::set_tools` /
`AgentCommand::SetTools` / `AgentHandle::set_tools`（fire-and-forget，邮箱
FIFO 保证紧随的 prompt 用新工具集）。与 ADR 前的 `set_system_prompt`
同一模式：静默替换、不发事件、下一次请求生效。goal 模式的换入/换回是
首个消费方；TUI 在启动时留存正常态工具集副本（`DynTool` 为 `Arc` 共享，
克隆廉价）作为换回基准。

### TUI：`goal <目标>` 启动 / `goal` 无参取消

- 命令参数是目标原文（自由文本，`goal 目标` 与 `goal:目标` 均接受）。
  启动属会话命令（须经 driver 换工具集 + 提交 prompt，运行中拒绝）；
  取消是本地命令（运行中可执行——追问状态立即解除，工具集经邮箱在
  本轮结束后替换）。
- 启动时目标包装为「目标驱动运行」提示词（自主推进、禁止提问、完成后
  调用 `goal_done`），作为 user 消息提交（聊天区可见、随 session 落库）。
- run 结束判定收在 `GoalNudger`（`nomic-tools/src/goal.rs`）：目标已完成
  → 换回正常工具集并通知；run 正常结束而未完成 → 以复述目标的提示词
  追问（计数 +1）；连续追问达上限（3 次）暂停，目标仍进行中，用户手动
  继续或取消。追问与队列的优先级沿用 ADR-0014 的口径：队列非空时队列
  优先。
- 会话切换（`new` / `resume` / `tree` 分支）取消进行中的目标并换回正常
  工具集：目标属于旧对话的上下文。
- 连带清理：`TodoStore::incomplete()` 的唯一消费方是旧 goal 模式，随本
  决策移除（含 `filter_incomplete` 与其测试）。

### web 入口：同一语义的 `/goal` 命令

- `SessionRuntime` 持有 `normal_tools` 与 `Mutex<GoalNudger>`（与 TUI
  复用同一 `GoalNudger` 实现）；`/goal <目标>` 空闲启动（运行中拒绝）、
  `/goal` 无参取消（运行中亦可），状态经 `goal_changed` 事件广播并入
  会话快照（`goal` 字段），前端以输入框上方的徽标行展示。
- 完成汇报复用消息流中的 `goal_done` 工具卡片；追问上限触顶经 `error`
  事件提示，不引入额外提示渠道。

## 后果

- 完成判定从「todo 清单是否清空」变为「模型显式汇报 + 用户可观察的
  工具调用」：判定权威与目标本身对齐，且完成汇报（summary）随工具
  结果进入聊天区与 session 落库，可回放。
- 目标驱动运行期间模型无法向用户提问：提示词要求信息不足时按最合理
  假设推进并在汇报中说明。
- 追问失控防护仍靠次数上限（3 次）；上限只暂停追问，不取消目标。
- 子 agent 池的工具集在配方组装期固定，不受主 agent 工具集替换影响
  （子 agent 仍可向用户提问）；如需一致语义，后续再提升为配方能力。
- print 入口（一次性非交互输出）无 goal 命令，行为不变。
