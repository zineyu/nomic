# ADR-0028: agent hooks 并入事件拦截（event interception）

- 状态：已接受
- 日期：2026-08-15
- 取代：ADR-0001 中「可选闭包 hooks 改为 `AgentHooks` trait」的工具执行挂点决策

## 背景

`nomic-core` 的生命周期同时存在两套机制，职责重叠：

- **`AgentEvent`**（事件流）：agent → turn → message → tool_execution 四层
  生命周期事件，经 `mpsc` 单向广播，TUI 渲染与 session 落库只**观察**、
  忽略返回值。
- **`AgentHooks`**（hooks）：`before_tool_call` 门控（`Allow | Block`）与
  `after_tool_call` 改写（`AfterToolCallOverride`），loop **直接 await** 其
  决策。

同一生命周期被拆成「观察」与「干预」两套词汇与两条调用路径：`ToolExecutionStart`
/ `ToolExecutionEnd` 事件已经承载了与 hook 上下文几乎一致的数据（工具 id / 名 /
参数 / 结果），hook 却另起一套 `BeforeToolCall` / `AfterToolCall` 上下文。

需求：**把 hooks 并入事件流**——拦截点用事件词汇表达，统一「观察」与「干预」，
消除重复机制；同时将单 hook 扩展为**多拦截器**（插入序、门控短路、改写 pipeline）。

## 决策

### 机制：拦截器由 loop 直接 await，观察仍单向广播

- 新增 `AgentInterceptor` trait，两个 per-event 拦截点（与事件同字段）：
  - `on_tool_execution_start(&ToolExecutionStart) -> ToolCallDecision`：门控；
    `Block { reason }` 时跳过执行，`reason` 作为错误 `ToolResult` 回喂模型。
  - `on_tool_execution_end(&ToolExecutionEnd) -> Option<ToolExecutionOverride>`：
    改写；字段逐项覆盖（`content` / `details` / `is_error` / `terminate`），
    未设置的保留原值。
- loop 在工具执行前/后直接 `await` 拦截器；观察者仍经既有 `AgentEvent`
  单向广播，观察端（TUI / session）**零改动**。`ToolExecutionStart` 仍先于
  门控广播，`ToolExecutionEnd` 仍携最终（改写后 / blocked 错误）结果，不新增
  blocked 事件。

### 多拦截器语义

- builder `.interceptor(Arc<dyn AgentInterceptor>)` 链式追加，按**插入序**执行。
- 门控：首个 `Block` **短路**（deny-wins），后续拦截器不再调用。
- 改写：**pipeline**——后一个拦截器看到前一个改写后的累积结果。
- 默认空 `Vec`（等价 noop）；保留 `NoopInterceptor` 作为显式默认与文档锚点。

### 负载对齐事件

拦截点上下文即事件负载：

- `ToolExecutionStart { tool_call_id, tool_name, args }`
- `ToolExecutionEnd { tool_call_id, tool_name, result, is_error }`

据此**不再暴露 `assistant_message` 与完整 `ToolCall`**（含 `thought_signature`）
——门控/改写的真实输入是「哪个工具 + 什么参数 + 什么结果」，且当前无调用方
使用 `assistant_message`。

### 命名

| 旧 | 新 |
|---|---|
| `AgentHooks` | `AgentInterceptor` |
| `NoopHooks` | `NoopInterceptor` |
| `before_tool_call` | `on_tool_execution_start` |
| `after_tool_call` | `on_tool_execution_end` |
| `BeforeToolCall` | `ToolExecutionStart<'a>` |
| `AfterToolCall` | `ToolExecutionEnd<'a>` |
| `AfterToolCallOverride` | `ToolExecutionOverride` |
| `ToolCallDecision` | 保留 |
| `.hooks()` | `.interceptor()` |

## 后果

- `AgentHooks` / `BeforeToolCall` / `AfterToolCall` / `AfterToolCallOverride`
  / `NoopHooks` 删除（pre-1.0，且无 `nomic-core` 之外的消费方，不留废弃垫片）。
- 拦截点能力收缩：`assistant_message` 与完整 `ToolCall` 不再暴露；若将来有
  拦截器需要完整 assistant 消息，可在 `ToolExecutionStart` 上按需扩展字段。
- 观察端行为完全不变：事件序列、`ToolExecutionStart` 广播时机、blocked 结果
  的回喂均与 hook 时代一致，本次替换对 TUI / session 透明。
- core 集成测试：`hook_block_produces_error_result_without_executing` 改名为
  `interceptor_block_produces_error_result_without_executing`；新增插入序、
  门控短路、改写 pipeline 三个多拦截器语义测试。
