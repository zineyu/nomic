# ADR-0007: typestate builder 完善 agent 创建

## Status

Accepted

## Date

2026-08-02

## Context

`Agent` 创建原依赖两个位置参数构造函数（`Agent::new` / `Agent::with_messages`）
加全字段必填的 `AgentConfig`：

- `AgentConfig` 六个字段全部必填，但其中四个有合理默认值
  （`stream_options`、`hooks`、`tool_execution`、`compaction`）。每个调用点
  （print 模式、TUI、测试）都重复手写 `Arc::new(NoopHooks)`、
  `ExecutionMode::Parallel` 等样板。
- 「哪些字段真的不可或缺」无法在类型层表达：`model` / `provider` /
  `system_prompt` 无默认值，与其余字段混在同一结构体里，漏设只能靠
  运行时（甚至根本不校验）。
- `new` / `with_messages` 两个入口的差异仅在于 messages 种子，
  位置参数重了调用方记忆负担。

## Decision

引入基于幽灵类型（`PhantomData` + `Set` / `Unset` 标记）的 typestate
builder `AgentBuilder<M, P, S>`，全面替代旧公开 API：

- **编译期强制必填项**：`model` / `provider` / `system_prompt` 各对应一个
  独立的类型参数，setter 消费 `self` 并翻转对应标记（`Unset → Set`）；
  `build()` 仅在 `AgentBuilder<Set, Set, Set>` 上实现——缺任一必填项则
  无法调用，类型错误即规格。三个标记相互独立，必填项设置顺序自由
  （每字段独立标记，而非线性状态链）。
- **默认值收敛到单一定义点**：tools/messages 为空、
  `stream_options` 为 `StreamOptions::default()`、hooks 为 `NoopHooks`、
  tool_execution 为 `Parallel`、compaction 为 `CompactionSettings::default()`，
  与旧调用点手写值一致。
- **`AgentConfig` 降为 crate 内部结构体**：`Agent` 运行时的字段分组不变，
  由 `build()` 组装；原 `with_messages` 实现迁移为 `pub(crate) from_parts`
  供 builder 调用。
- **session resume 场景**经 `.messages(history)` 可选 setter 表达，
  取代独立的 `with_messages` 入口。

不引入 trybuild 等编译失败测试依赖：typestate 的强制力是结构性的，
且额外 dev-dependency 会扩大 cargo-deny / cargo-machete 的审计面。

## Consequences

- 调用点删减样板（print / TUI / 测试均不再手写 NoopHooks/Parallel 默认值）；
  CLI 构造从嵌套结构体字面量变为扁平 setter 链。
- 新增必填创建项 = 加一个类型参数 + 一个翻转 setter，扩展路径清晰；
  新增可选创建项只加普通 setter，不影响类型签名。
- breaking change：`Agent::new` / `Agent::with_messages` / 公开的
  `AgentConfig` 移除，下游（当前仅 workspace 内 CLI 与测试）需迁移到
  `Agent::builder()`。
- `build()` 内部对必填项 `expect("typestate 保证已设置")`：panic 在
  类型层之外不可达，但保留显式断言以便阅读。
