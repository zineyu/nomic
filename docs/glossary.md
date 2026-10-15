# Glossary

> nomic 项目术语表。新确立的领域术语在此登记，全仓库文档统一使用。

## 架构

- **server**：常驻进程，SQLite 存储的唯一写者，agent loop 的宿主与
  监管者。独占数据库，负责 checkpoint/replay 路由、向客户端广播。
  本身不含 loop 逻辑，只做会话路由与生命周期管理。
- **会话 actor（session actor）**：每个会话一个的独占 tokio 任务 +
  命令邮箱（见 ADR-0007）。会话内串行、会话间并行；客户端订阅时
  按需拉起，启动时回放 checkpoint。
- **客户端（client）**：经 Unix socket + JSON Lines 协议连接 server
  的进程；TUI 是第一个客户端。客户端是「视图 + 输入」，无状态权威。
- **存储抽象（storage trait）**：server 依赖的存储接口；SQLite 是
  首个实现，未来可平滑增加 pgsql 等后端。
- **执行环境（execution environment）**：工具调用的派发目标抽象；
  工具定义与其解耦。初期仅本地执行环境。
- **消息队列串行化**：多客户端输入无锁排队、按到达顺序串行进入
  会话；无输入权锁、无固定主人。

## 会话与持久化

- **会话（session）**：与 agent 的一次连续对话，**树形结构**——可
  从任意消息分叉（fork），父子会话写时复制、相互独立。
- **checkpoint**：agent loop 每步（模型请求/响应、工具调用/结果、
  compaction）先落 SQLite 再推进；崩溃恢复与用户 resume 走同一条
  回放路径。
- **replay-safe**：工具声明自己崩溃后可安全重跑；不可重跑的工具
  恢复时标记 interrupted，已提交输出回灌模型，绝不重复执行副作用。
- **工作目录边界（workspace boundary）**：工具操作限定在会话绑定
  的启动工作目录内，目录外路径一律拒绝（含符号链接逃逸校验）。

## 工具与模型

- **工具（tool）**：agent 可调用的环境操作能力（读文件、写文件、
  shell 执行、列目录、内容搜索），带 replay-safe 元数据。
- **工具调用（tool call）**：LLM 请求执行工具的操作，loop 执行后
  结果回灌对话。
- **provider**：LLM 服务提供方，经 rig-core 适配器统一接入
  （见 ADR-0003）。项目内只允许经适配器使用 rig 类型。

## 交互

- **垂直切片（vertical slice）**：端到端可验证的最小功能路径。
- **TUI**：终端交互界面，首个客户端形态（见 ADR-0002）。
