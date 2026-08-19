# ADR-0031: 多 agent 并行 supervisor（fork-join 模式）

- 状态：已接受
- 日期：2026-08-19

## 背景

主 agent 经常需要将复杂任务拆分为多个子任务并行执行：研究 + 编码、
多文件分析、测试 + 文档等。此前所有工作都在单 agent loop 中串行
进行，无法利用多 LLM 调用的并行性。用户需要一种机制让主 agent
能动态创建独立子 agent、分配不同模型和工具、并行执行任务、汇总结果。

## 决策

### 核心抽象：`AgentSupervisor`

在 nomic-core 新增 `AgentSupervisor`，管理多个独立子 agent 的生命周期：

- **每个子 agent 是独立的 tokio actor**（`Agent::spawn()` → `AgentHandle`），
  拥有自己的消息历史、系统提示词、工具集、模型、事件流。
- **`AgentSupervisor`** 内部用 `RwLock<HashMap<AgentId, ChildAgent>>` 保护，
  支持并发操作（不同 agent 的 `wait_result` 可并发执行）。
- **配置**：`SupervisorConfig { max_agents }` 限制最大并发数（默认 8）。

### 模型选择

每个子 agent 可使用不同模型，在创建时由调用方（用户 / LLM）指定：

- `CreateAgentRequest::model` 为**必填项**（`Model` 类型）。
- `AgentSupervisor::new()` 接收 `available_models: Vec<Model>` 列表，
  传给工具层用于校验和展示。
- CLI 层通过 `ModelResolver::all_models()` 获取所有可用模型（基于
  `candidates()` + `resolve()` 转换）。

### 并发模型：非阻塞发送 + 阻塞等待

| 操作 | 阻塞性 | 实现 |
|------|--------|------|
| `send_message` | **非阻塞** | `tokio::spawn(handle.prompt())` 立即返回 |
| `wait_result` | **阻塞** | 取走 `JoinHandle` 并 await |
| `wait_all` | **阻塞** | `futures::future::join_all` 并发 await |

fork-join 典型流程：

```text
create_agent("a", model="claude-sonnet-4-20250514", ...)
create_agent("b", model="gpt-4o", ...)
send_message("a", "task A")   ← 非阻塞，a 的 LLM 调用在后台并行
send_message("b", "task B")   ← 非阻塞，b 的 LLM 调用在后台并行
wait_all(["a", "b"])           ← 并发等待，总耗时 = max(a, b)
close_agent("a")
close_agent("b")
```

### 工具层：6 个管理工具

在 nomic-tools 新增 `multi_agent` 模块，提供 6 个 `AgentTool` 实现：

| 工具名 | 阻塞性 | 说明 |
|--------|--------|------|
| `create_agent` | 否 | 创建子 agent（指定模型、系统提示词、工具子集） |
| `send_message` | 否 | 向子 agent 发送消息，立即返回 |
| `wait_result` | 是 | 等待子 agent 完成，返回 assistant 回复 |
| `wait_all` | 是 | 等待多个子 agent 全部完成 |
| `close_agent` | 否 | 关闭子 agent，释放资源 |
| `list_agents` | 否 | 列出所有子 agent 及其状态 |

工具构造函数 `multi_agent_tools(supervisor, available_tools)` 接收：

- `supervisor: Arc<AgentSupervisor>`——共享的 supervisor 实例。
- `available_tools: Vec<DynTool>`——可供子 agent 分配的工具池（**不含
  管理工具本身**，避免子 agent 递归创建子 agent）。

返回的工具列表直接传入主 agent 的 builder `.tools()`。

### CLI 集成

三个交互端（print / TUI / web）统一模式：

1. 创建 `AgentSupervisor`（共享 provider + 可用模型列表）。
2. 准备子 agent 工具池（基础工具，不含管理工具）。
3. 主 agent 工具集 = 基础工具 + `multi_agent_tools()`。
4. 主 agent builder 注入工具集。

`Bootstrap` 新增 `available_models: Vec<Model>` 字段，由
`ModelResolver::all_models()` 在启动时计算。

### 事件流

每个子 agent 有独立的事件流（build 时取得的 `UnboundedReceiver<AgentEvent>`）。
当前设计不聚合子 agent 事件到主 agent 事件流——主 agent 通过工具结果
（`wait_result` 返回的文本）获取子 agent 输出。未来如需 UI 展示子 agent
实时事件，可在 supervisor 层新增聚合通道。

### 资源回收

- `close_agent(id)`：abort 子 agent 的 prompt 任务（如有）+ actor 任务。
- `close_all()`：关闭所有子 agent（supervisor 析构或运行结束时调用）。
- actor 任务在全部句柄断开后自然退出（ADR-0022 同一口径）。

## 边界

- **子 agent 不拥有管理工具**：`multi_agent_tools()` 的 `available_tools`
  参数应排除管理工具本身，避免递归创建。
- **单 agent 场景不受影响**：不使用多 agent 工具时，supervisor 不被创建，
  零开销。
- **Agent 直接 API 不变**：supervisor 是新增层，不修改现有 `Agent` /
  `AgentHandle` 接口。

## 后果

- 主 agent 获得并行执行子任务的能力，总耗时从串行之和降为最慢子任务。
- 子 agent 可使用不同模型（如 reasoning 模型做分析，fast 模型做编码），
  由用户 / LLM 在创建时选择。
- 工具集可按子任务粒度分配（研究 agent 只有 read/grep，编码 agent 有
  read/write/edit/bash），实现最小权限。
- 新增 14 个 supervisor 集成测试覆盖完整生命周期。
- CLI 层工具数从 9 增至 15（+6 管理工具），主 agent 系统提示词需更新
  以引导正确使用。
