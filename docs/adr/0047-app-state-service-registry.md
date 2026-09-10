# ADR-0047: 应用状态（AppState）——Axum 风格的服务注册与传递

## Status

Accepted

## Date

2026-09-10

## Context

bootstrap 装配的全部组件（模型解析器 / provider / session 库 / skills /
提示词配方 / 候选模型与别名表……）此前以一个 15 字段的 `Bootstrap`
数据袋返回，三个入口（TUI / print / serve）各自解构后逐字段手工传递：
`spawn_driver` 12 个参数、`RecipeOpts` 10 个字段、serve 的
`SessionFactory` 复制了其中 10 个字段。问题：

- 新增一个进程级组件要同步修改 bootstrap 返回结构、三个入口的解构
  点与若干中间层签名，接线成本与遗漏风险随组件数线性增长；
- 组件间的依赖关系散落在长参数列表里，不可见也不可检查；
- 全局事件总线（ADR-0033）只在 serve 模式存在，由 `build_app_state`
  就地创建，TUI / print 侧没有可发布的总线句柄，「事件总线是进程级
  设施」这一事实在类型上不可见。

serve 的 HTTP 层本就用 axum——其 `State<S>` 机制（单一可 Clone 状态
对象注入 handler，handler 经 `FromRef` 只提取所需子状态）正是对内
部组件接线问题的既有答案。

## Decision

参考 Axum state 机制，新增进程级应用状态（`nomic-cli` 的 `state`
模块）：

- **`Services`**：服务集合，一个字段一个服务（store / 模型解析器 /
  provider / stream options / 提示词配方 / skills / 候选模型与别名表 /
  历史与 project / 消息总线……）。**注册即字段**：新增服务 = 新增
  字段 + 访问器，编译器指出所有需要接线的构造点。
- **`AppState`**（`Arc<Services>` 的可 Clone 句柄）↔ axum 的
  `State<S]`：在组件间传递；组件经**访问器**提取自己需要的服务
  （↔ `FromRef` 的子状态提取）。bootstrap 是唯一的真实装配点，
  `bootstrap()` 直接返回 `AppState`。
- **`EventBus`**：消息总线注册为 state 服务（`AppState::bus()`），
  事件类型复用 serve 协议的 `ServerEvent`（ADR-0033 的进程级全局
  事件总线语义不变）；提供 `publish` / `subscribe` / `sender`
  （常驻组件自持发送端副本）。三种模式的总线同在 bootstrap 创建，
  TUI / print 暂不发布，后续接入无需再改装配。

接线方式的变化（行为不变）：

- serve：`Runtime` 不再自持 `store` / `models` / `events` 字段，改持
  `services: AppState`；`SessionFactory` 同理（仅保留 serve 侧补充的
  `default_reasoning`）。原 axum 路由状态 `serve::AppState` 更名
  `WebState`，与进程级 `AppState` 区分。
- TUI / print：`spawn_driver` 与 `RecipeOpts` 接收 `AppState`，
  进程级服务（skills / 候选模型 / 别名表 / 模型解析器……）从 state
  提取；**session 级分化输入保留为显式参数**——serve 按 session
  解析的 provider / 默认模型、各入口的提问 sink 与 todo 策略、
  turn 注入点。
- TUI 的 `ModelSwitcher` 改持 `Arc<ModelResolver>`（与 state 共享
  同一解析器，设置重载天然同源）。

### 取舍：类型化 struct 而非 type-map

服务定位器式 type-map（`HashMap<TypeId, Arc<dyn Any>>`）支持运行期
动态注册，但依赖关系对编译器不可见、取用失败推迟到运行期 panic。
本项目的组件集合在编译期完全已知，类型化 struct 让「注册服务」与
「提取服务」都受编译器检查，与 workspace 的严格 lint 门禁一致。

## Consequences

- 新增进程级组件：bootstrap 装配 + `Services` 字段 + 访问器，三处
  一处都不能少（编译器保证）；消费方不再需要穿透中间层签名。
- `spawn_driver` 从 12 参数降到 8，`RecipeOpts` 从 10 字段降到 7，
  剩下的都是入口间 / session 间真正分化的输入。
- 事件总线在类型上成为进程级设施：`ServerEvent` 不再专属于 serve
  模块的私有创建点。TUI 侧未来接入统一事件流（如运行状态外发）
  只需 `state.bus()`，无需新建通道。
- 测试夹具逐字段构造 `Services`（serve/tests.rs），或用
  `AppState::stub`（cfg(test)）取最小状态（agent_recipe 测试）。
- `AppState` 目前覆盖进程级共享服务；session 级状态
  （`SessionRuntime` 注册表、停机令牌）仍属 serve 的 `Runtime`，
  不进入 `AppState`——两者的生命周期与共享范围不同。
