# ADR-0032: agent 配方组装模块（nomic-cli::agent_recipe）

- 状态：已接受
- 日期：2026-08-20

## 背景

ADR-0031 引入多 agent 形态后，「主 agent = 基础工具 + supervisor + 管理
工具，子 agent 池 = 基础工具」这套配方没有自己的模块——它以约 30 行
几乎相同的代码内联在 TUI（`tui/mod.rs`）、print（`print.rs`）、web
（`web/session.rs`）三个入口。各点的微妙差异是承载语义的，却不可见：

| 差异点 | TUI | print | web |
|--------|-----|-------|-----|
| todo 清单 | 主/子共享同一份（goal 模式与界面经共享句柄观察进度） | 主/子各自独立 | 主/子各自独立 |
| 基准目录 | 共享 `BaseDir` 句柄（resume/new 切换原地更新） | 固定 workspace | 固定 workspace |
| 提问通道 | `TuiQuestionSink`（弹层） | `StdinQuestionSink` | `WebQuestionSink`（事件总线） |
| turn 注入点 | 统一消息队列（ADR-0014） | 无 | 无 |

后果：新增工具或调整 supervisor 配置需要三处协调修改；nomic-tools 的
6 个 `default_tools*` 构造变体还要求每个调用点自己选对组合（是否带
skills、是否按路径/共享句柄）。

## 决策

在 nomic-cli 新增 `agent_recipe` 模块，对外只有一个入口
`assemble(RecipeOpts) -> AgentRecipe`：

- **入口差异经 `RecipeOpts` 显式表达**：`base`（`BaseDir` 句柄）、
  `skill_resolver`、`question_sink`（入口各自的 sink 适配器）、
  `todo`（`TodoPolicy::Shared(store)` / `Isolated`）、`provider` +
  `available_models`（supervisor 用）、`turn_injection`（`Option`，
  仅 TUI 为 `Some`）。
- **配方本身收进实现**：基础工具清单、管理工具接线、子 agent 池排除
  管理工具（避免递归创建）均为内部细节；`SupervisorConfig::default()`
  刻意不是选项——三入口当前一致，某入口需要分化时再提升。
- **6 个 `default_tools*` 变体收敛为内部唯一的共享句柄形式**
  （`default_tools_with_skills_in_shared`）：不需要原地更新的入口新建
  `BaseDir` 后不再写它，行为等同按固定路径构建。变体仍作为
  nomic-tools 的公共 API 保留（测试与潜在外部调用方），但 CLI 入口
  不再各自选择。
- **产物经 `AgentRecipe::apply(builder)` 装上 builder**：设置工具集，
  有注入点则一并设置；两者均非 typestate 必填项，`apply` 不改变
  builder 的类型状态，可插在 builder 链任意位置。model / system_prompt
  / history / stream_options 等仍是入口各自的 builder 接线（那是
  bootstrapping，不属于配方）。

## 位置选择

放在 nomic-cli 而非下沉 nomic-core：配方组合的是 nomic-tools（基础 +
管理工具）、nomic-skills（resolver）、nomic-core（supervisor）三方，
nomic-core 不依赖前两者（分层保持 core 不懂具体工具）；nomic-cli 已
依赖全部三方，且三个调用点都在此 crate 内。

## 后果

- 新增/调整基础工具或 supervisor 配置只需改 `agent_recipe` 一处；
  入口 PR 只在自己引入新的差异点时改动（提升为 `RecipeOpts` 选项）。
- 入口的差异从「内联代码里的微妙不同」变为「`RecipeOpts` 字面量里的
  显式字段」，语义可直接审查。
- `AgentRecipe` 不暴露 supervisor 句柄：当前无调用方在组装后使用它，
  生命周期由管理工具内部的 `Arc` 维持；未来需要（如退出时统一关闭
  子 agent）再开放。
