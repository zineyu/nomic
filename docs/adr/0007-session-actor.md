# ADR-0007: 多会话并发采用 actor 架构

## Status

Accepted

## Context

需求要求多个 session 同时进行。server 需要同时宿主多个 agent loop，
且每个会话内部保持串行语义（checkpoint 时序、消息队列）。并发组织
方式可选：共享状态 + 锁的并行 loop，或每会话一个 actor。

## Decision

**每个会话是一个 actor**：独占的 tokio 任务 + 命令邮箱（提交消息、
取消、订阅等经邮箱串行处理），持有本会话的状态与执行权；server
只做路由与监管（按会话 id 分发 IPC 消息、管理 actor 生命周期、
聚合订阅连接）。会话 actor 在客户端订阅时按需拉起，恢复路径（
checkpoint 回放）在 actor 启动时执行。TUI 保持单实例单会话，多
会话并发经多个 TUI 进程连接不同会话实现。

## Consequences

- 会话间天然并行、会话内天然串行，无需在 loop 上加锁。
- server 需要会话注册表与 actor 监管（崩溃的 actor 可被检测并标记）。
- 存储 trait 的调用方收敛为会话 actor（单写者模型按会话进一步细化）。
- TUI 单实例多会话（tab、后台通知）明确列为非目标。
