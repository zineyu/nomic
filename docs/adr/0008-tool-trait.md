# ADR-0008: Tool trait 与自动生成 JSON Schema

## Status

Accepted

## Context

规格 §4.2 的工具协议最初记录为「每个工具声明名称、JSON Schema
参数、replay-safe 标记」，schema 以原始 `serde_json::Value` 手写。
S1 评审指出三个缺口：

- 缺少 `Tool` trait：工具只有声明数据结构，没有类型化的实现契约；
- 手写 JSON Schema 与输入类型的实际结构是第二事实源，易漂移；
- 「工具自身报错」与「调用失败」没有在词汇层显式区分。

## Decision

工具协议改为 trait 化：

- nomic-core 定义 `Tool` trait，关联类型 `Input` / `Output` 声明
  工具的输入输出；input/output 的 JSON Schema 由 schemars 从关联
  类型自动生成（`Tool::spec()`），手写 schema 不再出现。
- 执行结果三层显式区分：`ToolResult::Ok`（成功输出）、
  `ToolResult::Error`（工具自身报错，回灌模型）、
  `ToolResult::Rejected`（执行前边界拒绝，原因对用户可见）；
  执行环境派发故障经 `EnvironmentError` 单独传播，与三层正交。
- `ToolError` 表达工具语义层面的失败（`InvalidInput` / `Failed`），
  实现者经 `From<ToolError> for ToolResult` 回灌。

## Consequences

- nomic-core 新增 schemars 依赖（MIT/Apache-2.0）；具体工具的
  Input/Output 类型同样以 schemars derive 生成 schema。
- S4（工具调用与本地执行环境）经 `Tool` trait 实现内置工具，
  执行环境持有工具注册表并按名称派发。
- `ToolSpec` 的 wire 形态携带 `input_schema` 与 `output_schema`
  两个字段，供 provider 协议层与客户端展示。
