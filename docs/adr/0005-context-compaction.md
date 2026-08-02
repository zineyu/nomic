# ADR-0005: 上下文压缩（compaction）

## Status

Accepted

## Date

2026-07-30

## Context

LLM 上下文窗口有限。长对话（尤其是编码 agent 的大量工具输出）会逼近窗口上限，
导致请求失败或被迫开新对话丢失上下文。pi 的解法是 compaction：把较早的消息段
压缩为结构化摘要，用一条合成 user 消息替换，保留近期消息原样。ADR-0001 已把
compaction 锚定为 M2 待办，本 ADR 记录 nomic 的实现决策。

需求确认的边界：

- 触发方式：**自动阈值触发 + `/compact [聚焦指令]` 手动命令**（pi 的完整行为）。
- 持久化：压缩结果**落库**（SQLite compaction entry），resume 后保持压缩状态。
- 与 pi 对齐默认值：`reserve_tokens = 16384`、`keep_recent_tokens = 20000`。

## Decision

### 分层

- `nomic-ai`：compaction 重建语义的唯一定义点（`nomic_ai::compaction` module）——
  摘要消息的构造与识别（`summary_message` / `is_summary_message` /
  `extract_summary` / `SUMMARY_PREFIX` / `SUMMARY_SUFFIX`）与有效上下文重建
  （`apply_compaction`：截尾 `kept_count` + 前置合成摘要）。包装文本逐字复刻 pi：
  `The conversation history before this point was compacted into the following
  summary:\n<summary>\n…\n</summary>`。该前缀同时是识别标记——放在消息模型层，
  因为 core（二次压缩提取 previous summary）、session（重建合成消息）、
  CLI（TUI 压缩渲染）三方都要判定；重建函数同层，因为 core（in-memory 组装
  新历史）与 session（resume 重放）必须共享同一实现才能逐字节一致。
- `nomic-core::compaction`：全部机制——token 估算、触发判定、切点、对话序列化、
  文件操作提取、摘要 LLM 调用。忠实复刻 pi 的算法与 prompt（逐字）：
  - 估算：chars/4（图片 4800 chars）；有 usage 时以最后一次有效 assistant 响应的
    `total_tokens`（缺省 `input + output + cache_read + cache_write`）为锚点，
    累加其后消息的估算。
  - 触发：`enabled && context_window > 0 && tokens > context_window - reserve_tokens`，
    每个 turn 开始前检查（压缩发生在 turn 之间）。
  - 切点：从最新往前累计到 `keep_recent_tokens`，切在最近的 user/assistant 边界；
    **永不切在 toolResult 前**（工具调用与结果必须同侧）。
  - 序列化：`[User]:` / `[Assistant]:` / `[Assistant thinking]:` /
    `[Assistant tool calls]: name(k=json)` / `[Tool result]:`（截断 2000 chars），
    防止模型把摘要请求当成对话继续。
  - 摘要请求与 agent 上下文完全隔离：专用系统提示词 + 单条 user 消息、不携带工具，
    其流事件**不进入** agent 事件流；输出上限
    `min(0.8 * reserve_tokens, model.max_tokens)`。二次压缩走 UPDATE prompt，
    前次摘要经 `<previous-summary>` 传入，不重复参与序列化。
  - 文件操作：从 read/write/edit 工具调用的 `path` 参数确定性提取
    `<read-files>` / `<modified-files>` 附加到摘要末尾；前次摘要中的清单解析合并，
    跨多次压缩累计。
- `nomic-core::agent`：`AgentConfig.compaction: CompactionSettings`；
  自动触发在 `run_loop` 每个 turn 前；手动 `Agent::compact(instructions, cancel)`
  供 `/compact` 调用，不受 `enabled` 开关限制。事件 `CompactionStart` /
  `CompactionEnd { summary, tokens_before, kept_count, usage }`。摘要失败返回
  `Err` 且历史不变（fail-safe）；自动路径仅告警继续。
- `nomic-session`：`entries.kind = 'compaction'` 的条目（migration 0002），
  payload 为 `CompactionRecord { summary, kept_count, tokens_before }`。
- `nomic-cli`：`[compaction]` 配置表（`enabled` / `reserve_tokens` /
  `keep_recent_tokens`，不设 CLI flag）；TUI `/compact` 命令（自由文本聚焦指令），
  `CompactionEnd` 落库；print 模式同样处理压缩事件（stderr 提示 + 落库）。

### 重建语义：`kept_count` 相对计数代替 `first_kept_entry_id`

pi 的 `CompactionEntry` 记录 `firstKeptEntryId`（绝对指针）。nomic 用
`kept_count`（压缩时保留的近期消息条数）：加载时沿默认分支重放，维护有效上下文
`Vec<Message>`——message 直接追加；遇 compaction 条目则截尾到 `kept_count` 条并
前置合成摘要消息。该递归语义对重复压缩天然成立（第二次的 `kept_count` 相对第一次
重建结果计数），且无需 entry id 簿记（CLI 落库时不存在 message→entry id 的映射）。

**已知限制**：该语义只对默认顺序分支成立。未来支持 branch 切换（ADR-0001 的树目标）
时需改回绝对指针——届时 compaction 条目需加记 `first_kept_entry_id`，
`load_messages` 按指针而非计数截尾。payload 是 JSON，届时加字段即可平滑迁移。

### 合成摘要消息不落库

压缩后的有效上下文 = 摘要 + 保留尾部，完全可以从 entries 重放重建，因此
合成摘要 user 消息**不作为 message entry 落库**（agent 也不为其发
`MessageStart`/`MessageEnd` 事件，既有落库管线自然不会写入）。resume 时由
`load_messages` 以 compaction 条目的 timestamp 重新合成，与 in-memory 表示逐字节一致。

## Alternatives Considered

### 压缩结果仅内存、不落库

- Pros：无 schema 变更；resume 后超长再重新压缩。
- Cons：每次 resume 都重新付一次摘要 LLM 调用的成本与延迟；摘要跨进程不稳定
  （同一会话两次 resume 可能得到不同摘要）。
- Rejected：需求方已确认落库；且 pi 的实战语义就是持久化。

### compaction entry 复用 `role` 列（不加 `kind` 列）

- Pros：零 schema 变更。
- Cons：`role` 是消息的提取列，塞入非消息语义是隐式契约；`kind` 显式区分条目
  种类，与未来更多 entry 类型（branch summary、custom）对齐 pi 的 `entry.type`。
- Rejected：一次 `ALTER TABLE` 的成本换取显式语义。

### split turn 双段摘要（pi 的完整行为）

单轮超长时 pi 切在 assistant 边界（split turn），对 turn 前缀单独生成
「给保留后缀的上下文」摘要再与历史摘要合并。

- Pros：超长单轮的摘要质量更好（聚焦后缀所需上下文）。
- Cons：两次 LLM 调用 + 合并逻辑；nomic 的切点实现同样允许 assistant 边界，
  只是整段一次摘要。
- Deferred：v1 整段一次摘要（prompt 相同）；质量不足时再补双段变体。

### Overflow 恢复（API 报 context-overflow 时压缩重试）

pi 在 provider 返回上下文溢出错误时自动压缩并重试该 turn。

- Deferred：需要 provider 层识别各家的溢出错误模式，与压缩重试的 loop 改造；
  阈值触发已覆盖绝大多数场景，列为 future work。

## Consequences

- 长会话在逼近窗口时自动续命，摘要质量与 pi 同契约（prompt 逐字复刻）。
- 每次压缩是一次额外 LLM 调用（输出上限 0.8 × reserve_tokens ≈ 13k tokens）；
  摘要请求的 usage 经 `CompactionEnd` 事件上报，暂未计入 session 成本统计。
- 无 prompt caching（ADR-0001 M2 待办），pi 的 `cacheRetention: "none"` 语义
  在 nomic 暂无为空操作；引入 caching 时摘要请求应禁止写缓存。
- TUI 聊天区把摘要消息压缩渲染为一行系统提示；完整摘要始终在上下文中，
  模型可见。
