# ADR-0005: 常驻 server + IPC 的多客户端架构，SQLite 存储

## Status

Accepted

## Context

需求要求多客户端围观/接管同一 agent 会话。两条路线：多进程直连共享
数据库（需外部 pgsql，并发写/通知/锁都在 DB 层解决）；或常驻 server
进程独占存储，客户端经 IPC 连接。

## Decision

- **nomic server** 常驻进程是存储的唯一写者，独占 SQLite 数据库。
- 客户端经 **Unix socket + JSON 协议** 连接 server；TUI 是第一个
  客户端，未来其他客户端复用同协议。
- 存储使用 SQLite（server 单写者模型下无并发写瓶颈，本地零部署
  负担）；先前用户提出的 pgsql 需求随本架构决策放弃。
- server 生命周期：客户端启动时发现 server 未运行则自动拉起
  （detached），随最后一个客户端退出可选关闭（具体策略见规格）。

## Consequences

- 多客户端并发由 server 进程内解决：所有客户端输入消息无锁排队、
  串行进入会话；实时推送广播给全部连接。
- TUI 不再是完整应用，退化为「视图 + 输入」，状态权威在 server。
- 崩溃恢复语义（checkpoint/replay）在 server 进程内闭环。
- **存储 trait 化**：server 只依赖存储抽象（trait），SQLite 是首个
  实现；未来可平滑增加 pgsql 等其他后端，server 代码无感知。
