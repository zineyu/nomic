# ADR-0001: nomic 总体架构 — Rust 复刻 pi-coding-agent

## Status

Accepted

## Date

2026-07-25

## Context

nomic 目标是以 Rust 复刻 [pi-coding-agent](https://github.com/badlogic/pi-mono)（下称 pi）的核心能力：
一个可嵌入的编码 agent harness —— 统一的多 provider LLM 流式抽象、agent loop、
read/write/edit/bash 工具、session 持久化，以及其上的 CLI。

pi 是 TypeScript 实现，分为 `pi-ai`（provider 抽象）、`pi-agent-core`（agent loop）、
`pi-tui`（终端 UI）、`pi-coding-agent`（工具 + 模式 + 定制）四层。我们借鉴其经过实战检验
的设计决策，但不追求一比一复刻：凡 Rust 生态有更自然表达的，采用 Rust 风格实现。

已与需求方确认的边界：

- LLM 层**自研 provider 抽象**，不使用 rig（移除既有 rig 依赖）。
- M1 范围：核心 agent loop + read/write/edit/bash 四工具 + print 模式 CLI（非交互管道可用）。
- session 持久化用 **SQLite**（M2），不照搬 pi 的 JSONL 文件。
- extensions（pi 的 TS 动态加载插件）**不做**；声明式定制（skills/prompt templates/AGENTS.md）后续里程碑再做。
- 优先 provider：**Anthropic Messages API** 与 **OpenAI Completions 兼容端点**（不做 Responses API）。
- M1 配置仅走环境变量（`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `OPENAI_BASE_URL`）+ `--model` 标志，不引入配置文件。

## Decision

### Crate 划分（workspace）

```
crates/
  nomic-ai      # 消息模型 + 流式协议 + provider 实现（对应 pi-ai）
  nomic-core    # agent loop + AgentTool trait + hooks（对应 pi-agent-core）
  nomic-tools   # read/write/edit/bash + 截断工具（对应 pi-coding-agent 的 tools）
  nomic-cli     # 二进制：print 模式（-p/--print），后续加交互模式
```

依赖方向单向：`nomic-cli → nomic-tools → nomic-core → nomic-ai`。
所有消息与配置类型从 M1 起派生 `Serialize`/`Deserialize`，保证 M2 的 SQLite session
落地时无类型 churn。

### 消息模型（借鉴 pi-ai）

- `Message = User | Assistant | ToolResult` 三种角色。
- Assistant 内容为有序内容块：`Text | Thinking | Image | ToolCall`；
  thinking 块保留 `signature`（Anthropic 多轮续传必需）。
- `Usage { input, output, cache_read, cache_write, reasoning?, cost }`，
  `StopReason { Stop, Length, ToolUse, Error, Aborted }`。
- `ToolDefinition { name, description, parameters: serde_json::Value(JSON Schema) }`。

### 流式协议（偏离 pi 的最大决策）

pi 的每个 delta 事件都携带完整的 partial `AssistantMessage` 快照 —— JS 中廉价，
Rust 中是逐 token 的堆分配。**nomic 的事件只携带增量**：

```rust
enum AssistantEvent {
    Start,
    TextStart    { index: usize },
    TextDelta    { index: usize, delta: String },
    TextEnd      { index: usize },
    ThinkingStart{ index: usize },
    ThinkingDelta{ index: usize, delta: String },
    ThinkingEnd  { index: usize },
    ToolCallStart{ index: usize },
    ToolCallDelta{ index: usize, delta: String },   // partial JSON
    ToolCallEnd  { index: usize, tool_call: ToolCall },
    Done         { message: AssistantMessage },      // stop_reason ∈ Stop|Length|ToolUse
    Error        { message: AssistantMessage },      // stop_reason ∈ Error|Aborted
}
```

provider 任务内部用 builder 累积完整消息，`Done`/`Error` 一次性交付。
需要 partial 状态的消费方（未来的 TUI）自行按 `(index, delta)` 累积 —— 廉价且无损。

### 错误契约（原样保留 pi）

`Provider::stream()` 对请求/运行时失败**不返回 `Err`**；失败编码为终止事件 `Error`，
携带 `stop_reason = Error | Aborted` 与 `error_message` 的完整 `AssistantMessage`。
这使 agent loop 的错误路径线性化（loop 无需区分"流建立失败"与"流中途失败"）。
真正的 bug（参数构造错误等）仍可 panic/Err —— 契约只覆盖运行时失败。

### Tool 抽象（Rust 风格）

pi 用 typebox 定义 schema + `validateToolArguments` 运行时校验。nomic 用
**schemars + serde 反序列化即校验**：

```rust
#[async_trait]
trait AgentTool: Send + Sync {
    type Params: DeserializeOwned + JsonSchema + Send;
    type Output: Serialize;

    fn name(&self) -> &'static str;
    fn label(&self) -> &str;
    fn description(&self) -> &str;
    fn execution_mode(&self) -> ExecutionMode { ExecutionMode::Parallel }

    async fn execute(
        &self,
        params: Self::Params,
        cancel: CancellationToken,
        on_update: Box<dyn Fn(ToolUpdate) + Send>,
    ) -> Result<ToolResult, ToolError>;
}
```

dyn 兼容通过一层 type-erased 包装（`DynTool`，内部 `BoxFuture` + `serde_json::Value`
参数中转）。工具执行失败 → `is_error = true` 的 `ToolResult` 回喂模型（与 pi 一致：
错误是给模型的信号，不是给 loop 的异常）。

### Agent loop（借鉴 pi-agent-core，M1 裁剪）

保留：agent/turn/message/tool_execution 四层生命周期事件；parallel 工具执行
（`futures::join_all`，per-path 文件变更串行化 —— 写同一文件的真正风险点）；
`stop_reason == Length` 时批量失败该消息所有工具调用（参数可能被截断，执行不安全）；
`terminate` 提前终止提示。

M1 裁掉（事件枚举预留形状，后续里程碑加入）：
steering/follow-up 消息队列、`prepareNextTurn`、`shouldStopAfterTurn`。

Hooks 用 **trait + 默认空实现**（`AgentHooks`），而非 pi 的一堆可选闭包字段；
M1 只实现 `before_tool_call` / `after_tool_call`（权限门控的挂点）。

取消用 tokio-util 的 `CancellationToken`（替代 AbortSignal）。

### Provider 实现要点

- HTTP：reqwest（**rustls-only**，关闭 native-tls，保持依赖树干净过 cargo-deny）。
- SSE：参考 pi 的 ~60 行行解码器手写（可控缓冲），或 `eventsource-stream`，实现时择优。
- Anthropic：Messages API 流式；M1 不做 prompt caching（`cache_control`），
  但请求构建器设计为后续可附加（M2 最大成本杠杆）。
- OpenAI 兼容：仅 Completions API（`stream: true` + `stream_options.include_usage`）。
  从第一天就建 `OpenAiCompat` 结构体承载兼容性差异，但只建模会真实碰到的字段
  （`max_tokens` vs `max_completion_tokens`、流中 usage 支持、developer/system role），
  不照搬 pi 的 20+ 字段全集。
- 测试：**fixture 回放** —— 录制真实 Anthropic/OpenAI SSE 流转储，测试中回放。
  测试不打真实 API。

### 工具语义（忠实复刻 pi 的模型契约）

工具的"系统提示词契约"是 pi 质量的关键，逐条复刻：

- `read`：offset/limit；头部截断（2000 行 / 50KB 先到先赢）；截断时输出
  `[Showing lines X-Y of N. Use offset=Z to continue.]` 引导模型翻页；图片作为附件内容块（M1 可先仅文本）。
- `write`：自动创建父目录；per-path 变更队列串行化。
- `edit`：`edits[]` 多处精确替换，对原始文件匹配（非增量）；模糊匹配归一化
  （行尾空白、智能引号、Unicode 破折号/空格）；保留 BOM 与 CRLF；返回 diff/patch details；
  唯一性校验（oldText 多处匹配 → 报错给模型）。
- `bash`：尾部截断，完整输出落临时文件并在截断提示中给出路径；超时；退出码非零 → 错误结果
  （输出附加 `Command exited with code N`）；100ms 节流进度更新。

### CLI（M1）

`nomic -p "prompt"` print 模式：流式输出到 stdout，工具执行摘要到 stderr，
退出码反映成功/失败，管道可用。配置纯环境变量 + `--model` / `--provider`。

## Alternatives Considered

### 基于 rig 构建

- Pros：多 provider 开箱即用，上手快。
- Cons：抽象层与 pi 的消息/流式模型不一致（thinking blocks、tool call 流式增量、
  统一 Usage/cost、错误编码契约都受限于 rig 的模型）；深入后必然绕开。
- Rejected：需求方已确认自研。

### 流事件携带 partial 消息（完全照抄 pi）

- Pros：消费方实现最简单。
- Cons：Rust 中每 token 克隆整棵内容树，无意义分配；消费方自行累积 delta 仅十几行代码。
- Rejected：见"流式协议"节。

### session 用 JSONL（照抄 pi）

- Pros：与 pi session 文件互通；实现简单。
- Cons：需求方已选 SQLite（查询、并发、树遍历更好）；且 nomic 不追求与 pi 文件互通。
- Rejected：M2 用 SQLite（sqlx 或 rusqlite，届时再定）。

## Consequences

- 需要自建 SSE 解析与两个 provider 的消息变换（约 1.5k 行），换取对协议的完全控制。
- 移除 rig 依赖；workspace 从单 binary 变为多 crate。
- 所有测试基于 fixture，CI 无需 API key。
- M2 待办清单（已在 ADR 中锚定）：SQLite session（树结构 + branching）、prompt caching、
  compaction、skills/prompt templates/AGENTS.md 加载、交互 TUI、图片输入。

## 借鉴 vs 偏离 对照表

| pi 设计 | nomic 决策 | 理由 |
|---|---|---|
| delta 事件携带 partial 消息 | 事件只带增量，Done 交付完整消息 | 避免逐 token 克隆 |
| AbortSignal | tokio `CancellationToken` | Rust 异步生态惯例 |
| 可选闭包 hooks | `AgentHooks` trait 默认空实现 | 类型安全、可扩展 |
| typebox 运行时校验 | schemars + serde 反序列化即校验 | Rust 类型系统原生能力 |
| 错误编码进流（StreamFunction 不抛） | 原样保留 | loop 错误路径线性化，实战验证 |
| 工具截断契约（2000行/50KB、翻页提示） | 原样保留 | 模型行为质量的关键 |
| 模糊匹配 + BOM/CRLF 保留的 edit | 原样保留 | 同上 |
| parallel 工具执行 + 文件变更队列 | 原样保留 | 模型常批量发调用 |
| JSONL session 文件 | SQLite（M2） | 需求方选择；查询/并发更好 |
| TS extensions | 不做 | Rust 无法照搬动态加载；声明式定制先行 |
| OAuth 订阅登录 | 不做（M1 仅 API key 环境变量） | 工作量与价值不匹配 |

## Amendments（现状修订，不改写历史决策）

### 2026-07-27：实现进度与决策漂移说明

ADR-0001 的里程碑边界描述已成历史，以下为当前实际状态，阅读本文时请以此为准：

- **配置文件**：「M1 不引入配置文件」已被取代 —— 现已支持
  `$XDG_CONFIG_HOME/nomic/config.toml`，优先级 CLI 参数 > 环境变量 > 配置文件 > 内置默认。
- **交互 TUI**：已由 [ADR-0002](0002-interactive-tui.md) 落地（ratatui），`nomic-cli`
  不再是纯 print 模式。
- **依赖方向**：实际为组装式而非严格链式 —— `nomic-cli` 直接依赖
  `nomic-ai` / `nomic-core` / `nomic-session` / `nomic-tools`；core/tools/ai 之间仍保持单向。
- **消息模型**：`AssistantContent` 实际为 `Text | Thinking | ToolCall`（无 `Image`）；
  图片只存在于 `UserContent`。
- **session**：SQLite 存储（`nomic-session`）已实现并接入 CLI 的创建/落库/resume；
  树形 schema（`parent_id`）上的**显式 branching 已落地**：TUI `/tree` 命令浏览
  会话树（树形前缀画出分叉，线性链平铺，连续工具条目折叠为摘要行），选择
  非工具调用条目作为新分支起点——沿祖先
  路径重放恢复上下文，后续消息以该条目为父 entry 落库（落库父指针随每次写入
  推进），原分支保留可回访。工具结果与含工具调用的 assistant 条目不可选（避免
  悬空 tool_use 进入上下文）。分支命名/active leaf 等语义仍未定义，默认分支
  维持「每级最新子节点」。
- **session 恢复语义**：`--continue` 按当前 cwd 隔离恢复（只选本目录最近的 session，
  避免跨项目误恢复）；`--session <ID>` 可显式跨目录恢复并有提示；新增
  `nomic sessions list` 子命令。
- **skills**：「skills/prompt templates 后续里程碑再做」中 skills 已由
  [ADR-0003](0003-skills-system.md) 落地 —— 新增 `nomic-skills` crate，支持项目/
  用户/通用 agent 目录发现、frontmatter 元数据、system prompt 清单、CLI `--skill`
  显式激活，以及 `read` 的只读 `skill://<name>` 分页读取。prompt templates 仍未实现。
- **测试策略**：「所有测试基于 fixture」不再准确 —— core/tools/session/CLI 均有
  直接构造的单元与集成测试（含进程级 CLI 测试）；provider 协议层仍为 fixture 回放。

### 2026-08-15：hooks 被事件拦截取代

「可选闭包 hooks 改为 `AgentHooks` trait」的工具执行挂点决策已被
[ADR-0028](0028-agent-hook-to-event-interception.md) 取代：hooks 并入事件流，
`AgentHooks` → `AgentInterceptor`（多拦截器插入序 / 门控短路 / 改写 pipeline）。
阅读 hooks 相关段落时以 ADR-0028 为准。
