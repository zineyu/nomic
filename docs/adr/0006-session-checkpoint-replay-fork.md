# ADR-0006: 会话模型——逐步 checkpoint、replay-safe 工具、分叉

## Status

Accepted

## Context

参考 Pi Durable 的持久化语义：模型调用、工具调用、compaction 都先
落 checkpoint 再推进；工具需声明 replay-safe 才允许崩溃后自动重跑；
用户同时要求多会话分叉能力。

## Decision

1. **逐步 checkpoint**：server 内 agent loop 的每一步（模型请求/
   响应、工具调用/结果、compaction）先写入 SQLite 再推进；进程崩溃
   后重启从同一存储回放继续。用户主动 resume 与崩溃恢复走同一条
   回放路径。
2. **replay-safe 声明**：每个工具声明自己是否可在崩溃后安全重跑；
   不可重跑的工具在恢复时被标记为 interrupted，已提交的部分输出
   回灌模型，绝不悄悄重复执行副作用。
3. **会话分叉（fork）**：会话是树形结构，可从任意消息分叉出新
   会话（父子共享历史、写时复制）；分叉详情（分叉点规则、列表
   形态）见规格。
4. 工具与执行环境解耦：工具定义不绑定执行方式，经执行环境抽象
   派发；初期只有本地执行环境（见规格）。

## Consequences

- SQLite schema 需表达消息树（parent 指针）而非线性列表。
- 工具注册需带 replay-safe 元数据。
- 外部副作用（支付、发消息类）若未来接入，幂等仍由工具自身负责，
  checkpoint 只保证提交层不重复受理。
