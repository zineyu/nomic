# ADR-0023: 落库策略收进 SessionRecorder（事件流 seam 之后）

## Status

Accepted

## Date

2026-08-14

## Context

session 落库的策略——「何时落库（`MessageEnd` / `CompactionEnd` 定稿点）、
落什么（消息 payload / `CompactionRecord`）、父指针怎么推进」——由 print
与 TUI 两个调用端各自实现，且语义已经漂移：

- print 模式恒用 `parent=None`（自动链最新 entry），不维护父指针；
- TUI 在 `Driver` 上自维护 `tip` 字段：落库成功推进、`/tree` 分支切换、
  `/new` 重置、`/resume` 恢复为默认分支末端，散在 driver 与
  `effects/session.rs` 多处。

分支语义两侧不一致，修一处不会修另一处；落库 bug 也无法不起 TUI
直接复现。

seam 已经存在：两个调用端都消费同一条 `AgentEvent` 流（ADR-0022 actor
模型未改变事件推送模型）。策略只需要挪到 seam 后面。

## Decision

nomic-session 新增 `SessionRecorder`（`recorder.rs`），持有
`SessionStore` + 目标 session id + 父指针（tip）：

- **`record(&AgentEvent)`**：唯一定稿点判定——`MessageEnd` 追加消息、
  `CompactionEnd` 追加压缩条目，均以当前 tip 为父；成功后推进 tip 到
  新 entry，失败不推进（store 非权威源，提示方式由调用端决定）。其余
  事件忽略。
- **tip 管理**：`set_tip`（`/tree` 从所选条目创建分支）、`switch`
  （`/new` 重置为自动链最新、`/resume` 恢复为默认分支末端）。
- **调用端接线**：print 的 `drain_events` 与 TUI 的 `Wake::AgentEvent`
  分支各做一行 `recorder.record(&event)`，失败就地告警；TUI 的
  `persist` / `persist_compaction` 与 `Driver::tip` 字段删除。

### 模块边界

`SessionRecorder` 放在 nomic-session 而非 nomic-core，为此 nomic-session
新增对 nomic-core 的依赖（仅用 `AgentEvent` 类型）：

- nomic-core 保持对持久化零感知（store 非权威源的设计不变）；若反向
  让 nomic-core 依赖 nomic-session，等于把存储策略塞回 agent runtime。
- 依赖方向 nomic-session → nomic-core 不构成环（core 仅依赖 nomic-ai），
  且符合「session 是 agent 事件流的下游消费者」的分层。

## Consequences

- 落库策略单一实现：locality（bug 集中于 `recorder.rs`）、leverage
  （一个 interface，两个调用端）、父指针语义被迫对齐。
- 测试直接打在事件流上（`tests/recorder.rs`：父链推进、分支切换、
  失败不推进、非定稿点忽略、session 换绑），不起 TUI。
- print 模式行为微调：从「恒自动链最新」变为与 TUI 一致的 tip 推进；
  单进程独占写入时两者等价，但语义不再有两份解释。
- nomic-session 编译期多带一个 nomic-core 依赖；`AgentEvent` 定义若
  下沉 nomic-ai 可进一步解开，当前不值得为此移动事件类型。
