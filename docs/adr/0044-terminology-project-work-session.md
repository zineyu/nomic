# ADR-0044: 术语统一：project / work / session 三层模型

- 状态：已接受
- 日期：2026-09-03
- 修订：ADR-0031（多 agent 协作——子 agent 从纯内存 actor 升级为落库 session）
- 被修订：历史 ADR 正文中的 `workspace` 字样统一改写为 `project`（代码与
  文档使用同一词汇，历史决策记录以现行术语为准）

## 背景

仓库长期混用 workspace / session / 会话 等词：`Workspace` 实为「一个 git
仓库目录」，session 同时承担「单次 agent 运行上下文」与「用户的一次任务
协作」两种含义；多 agent（ADR-0031）引入后，子 agent 的对话无处安放——
纯内存 actor，进程退出即消失，既无法回溯，也与「session 是唯一持久化
单元」的存储模型矛盾。

本 ADR 确立三个术语及其层级关系，并落地为代码、存储与 UI 的统一模型。

## 决策

### 术语定义

| 术语 | 定义 | 例 |
| --- | --- | --- |
| **project** | 一个 git 仓库目录：AGENTS.md / skills 发现、工具相对路径、session 隔离的基准（原 `workspace` 更名） | `~/space/project/nomic` |
| **work** | 一次任务协作过程：用户的一等入口，是 session 的分组容器 | 「给登录页加验证码」 |
| **session** | 一次 agent 交互上下文：消息流、compaction（ADR-0005）、落库与分支（ADR-0023）的最小持久化单元 | 主 session、子 agent session |

层级：**project 1—N work 1—N session**，全部 NOT NULL 外键；session 不再
直接持有 project 外键（经 work 派生，`JOIN works`）。

### work 是一等入口

work 占据原先 session 在产品语义中的位置：CLI `sessions list` /
`nomic resume` / `--continue`、TUI `/resume`、web 侧栏列表均以 work 为
行单位；work 行携带 `main_session_id`（首个 session 即主 session），
「打开 work」= 恢复其主 session，既有 resume 路径不变。

新建对话 = 创建 work（连带主 session）；删除 work 级联删除名下全部
session 与消息；重命名作用于 work。session 粒度的管理操作不暴露。

### 子 agent 落库为同 work 下的 session（修订 ADR-0031）

`create_agent` 创建的子 agent 在父 session 所属 work 下落库为一个
session：

- `sessions.parent_session_id` 记录血缘（`NULL` = 主 session）；
- 标题沿用派生规则（首条 user 消息 = 父 agent 下达的任务摘要）；
- 独立 entry 树，与父 session 互不干扰；
- **只读回溯，不可 resume**：web 前端禁用输入框，后端在打开运行时一次
  性判定血缘并同步拒绝 prompt（不引入调度点，见 handler 测试）；
- 不单独删除，随 work 级联删除；
- 无持久化的运行（store 不可用）不退化：hook 为 `None`，事件流随 close
  丢弃（旧行为）。

fork-join 语义（ADR-0031）不变：supervisor 仍管理内存中的 actor 生命
周期；落库是附加观察方——`AgentSupervisor::take_events` 让调用端
（`create_agent` 工具经 `ChildSessionHook` 接缝）在 create 成功后接管
事件流一次，nomic-cli 装配时注入 hook（创建子 session + spawn 落库任务
消费至通道关闭）。

### 迁移与硬切换

- 迁移 `0009`：`workspaces` 表更名 `projects`；迁移 `0010`：新建
  `works` 表并把既有 session 1:1 回填（work id = `w-<session id>`，
  主 session 指向自身）；
- 旧迁移文件（0001–0008）不改：sqlx 内嵌校验和，编辑会破坏既有库；
- 无别名/兼容层：协议字段、类型名、文案一次性切换（pre-release 项目）。

### 排除项

- Cargo 工具链语义的 `workspace`（`--workspace`、`[workspace.*]`、
  `foo.workspace = true`）不在更名范围；
- `SkillScope::Project` / `PromptScope::Project` 枚举名不变（语义恰与
  新术语收敛）。

## 结果

- 三个术语在代码标识符、SQLite schema、WS 协议、web 组件与文案、
  README/CHANGELOG/历史 ADR 中一致；`workspace` 仅残留在 Cargo 语境
  与旧迁移文件中；
- work 成为多 agent 对话的天然容器：主/子 session 在 work 视图内并列，
  用户可回溯子 agent 的完整对话；
- 后续演进（work 元数据、work 级统计、子 agent 嵌套展示）以本 ADR 的
  层级为骨架。
