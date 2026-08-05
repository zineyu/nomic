# ADR-0004: 模型规格分层解析（配置 → models.dev → 内置默认）

## Status

Accepted（「内置默认」层已由 ADR-0010 移除，改为协议级中性兜底；其余分层不变）

## Date

2026-07-30

## Context

`Model` 有 12 个字段，此前除 `id` / `api` / `provider` / `base_url` 外的规格字段
（`name` / `reasoning` / `context_window` / `max_tokens` / `cost_*`）全部硬编码在
`resolve_model` 的 provider 预设里：只对 anthropic / openai 两个内置 provider 有正确值，
任何兼容端点（deepseek、自建网关等）都拿到错误的上下文窗口与费率，成本统计和
上下文管理随之失真；用户也无法修正。

需求确认的分层语义：

- 所有字段优先采用配置中的数据；
- 配置缺失时通过 models.dev 获取；
- models.dev 也无法获取时使用全局默认值（即既有内置预设，保证离线行为不回退）；
- `provider` 与 `base_url` 必须由用户指定，**不**从 models.dev 获取——
  models.dev 只按模型 id 提供其余规格字段。

## Decision

### 逐字段分层，而非整体预设选择

`resolve_model` 从「按 provider 选一套预设」重构为逐字段解析：

```text
规格字段：providers.<名字>.models."<模型id>".<字段> → models.dev → 内置预设
连接字段：CLI 参数 → 环境变量 → providers.<名字>.* → 平铺配置 → 内置默认
```

每层都是部分结构（`ModelSpec`，8 个可选字段），用 `or_fill` 逐字段合并，
上层已给值的字段不被下层覆盖。

### models.dev 集成（`nomic-ai::models_dev`）

- 数据源为 `https://models.dev/api.json`（约 3MB，provider → models 嵌套）；
  只取 `name` / `reasoning` / `limit.{context,output}` / `cost.*` 六个字段。
- 查询以模型 id 为准：优先在用户配置的 provider 键下匹配（同一模型 id 可能被
  多个 provider 以不同费率提供），未命中时全局扫描首个匹配。provider 键只影响
  匹配优先级，其值永远来自用户配置。
- 磁盘缓存 `$XDG_CACHE_HOME/nomic/models-dev-api.json`，24h TTL；网络拉取 3s 超时，
  失败时用过期缓存兜底，缓存与网络均不可用时返回 `None` 落内置预设。
  配置已给全 8 个规格字段时跳过整个加载（不读缓存、不联网）。
- 逐 provider / 逐模型容错解析：models.dev schema 不受本仓库控制，单个脏条目跳过，
  不拖垮整个目录。

### 配置结构：`[providers.<名字>]` 嵌套 `models`

- `providers.<名字>`：`api` / `base_url` / `api_key` / `models`。
  `api` 对 anthropic / openai 自动推断，自定义 provider 必填（加载期硬报错）。
- 复用顶层 `provider` / `model` 作为选择器；`provider` 取值必须是内置名字或
  `[providers]` 表中定义的键，CLI `--provider` 同步放开自定义名字。
- 规格字段不与既有平铺键复用：顶层 `reasoning` 是思考级别（minimal/low/...），
  顶层 `max_tokens` 是请求参数，与模型能力语义不同，嵌套表避免命名冲突。
- 平铺 `base_url` / `api_key` 保留兼容，优先级低于 `providers.<名字>.*`。

### 模型存在性校验（禁止配置不存在的模型）

`resolve` 只对「已知模型」放行：命中 `providers.<名字>.models` 定义、models.dev 目录
（按模型 id 全局扫描）或内置默认模型之一，否则硬报错。启动路径（CLI `--model` /
配置 `model`）直接失败，`/models:<id>` 运行时切换转为提示。

模型 id 是规格解析的唯一键，校验跟随分层口径：配置覆盖表永远算「存在」（用户显式
定义）；目录不可用（离线 / 配置已写全规格跳过加载）时没有权威数据源，无法判断
「不存在」，维持既有回落语义（启动告警 + 内置预设），仅在有目录可查时严格拒绝。

### 自定义 provider 的中性预设

内置 provider 保留既有预设（anthropic 200k/64k/费率、openai 400k/128k/零费率）。
自定义 provider 没有可靠的猜测依据，预设为中性值（reasoning=false、窗口 0、
费率 0、无默认模型 id——必须显式指定模型）。

## Consequences

- 兼容端点（deepseek 等）开箱即有正确的上下文窗口与费率，成本统计可信。
- 离线 / models.dev 不可达时行为与今天完全一致（内置预设兜底），仅有启动时一条告警。
- 每次启动最多一次 3s 超时的网络请求；缓存新鲜时只有一次本地读 + 解析（约几十毫秒）。
- models.dev 的 cost 数据为展示价，可能与实际计费（折扣、阶梯价）不一致；
  需要精确成本的用户可用 `[providers.*.models.*]` 显式覆盖。
