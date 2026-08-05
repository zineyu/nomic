# ADR-0009: 配置迁移到 sqlite（模型选择先行，append-only 回退链）

## Status

Accepted

## Date

2026-08-05

## Context

模型与 provider 的**选择**此前放在配置文件（`provider = ...` / `model = ...`）里，
但选择是高频变动的状态而非静态配置：用户在 TUI 里用 `/models` 切换模型后，
下次启动仍按配置文件里的旧选择走，除非手动编辑文件。同时 `/models` 只能在
当前 provider 内切换，跨 provider 切换必须改配置并重启。

需求确认的语义：

- 模型和 provider 的选择从配置文件中移除，改由命令选择（`/models`，
  可跨 provider，选择项为 `<provider>/<模型id>` 格式），结果保存在数据库中；
- sqlite 增加专用配置表，包含更新时间戳；每次修改配置新增一行；
- 引入 feedback 机制：从最新的配置向最老的配置逐步回退，直到无可回退；
- 配置值用 sqlite 原生 JSON 类型存储；
- 配置文件与 sqlite 配置暂时共存，未来逐步移除配置文件配置。

## Decision

### config 表：append-only 配置历史（nomic-session，migration 0003）

```sql
CREATE TABLE config (
    id         INTEGER PRIMARY KEY,
    "key"      TEXT NOT NULL,       -- 配置键（如 "model"）
    value      JSONB NOT NULL,      -- sqlite 原生 JSON（JSONB 二进制，需 SQLite >= 3.45）
    updated_at INTEGER NOT NULL     -- 更新时间戳（Unix 毫秒）
);
```

- **append-only**：`set_config` 每次修改 `INSERT` 新行，不回写旧行；
  历史本身就是回退链，无需单独的撤销机制。
- **JSONB 原生类型**：写入经 `jsonb(?)` 校验并以二进制 JSON 存储，读取经
  `json(value)` 归一化为文本 JSON。sqlx 的 `sqlite` feature 启用 bundled
  SQLite 3.46，JSONB 可用。
- 表是通用的键值配置存储；第一个消费方是模型选择（`key = "model"`，
  值为 `"<provider>/<模型id>"` 字符串）。

### feedback 回退：两层

- **存储层**（`SessionStore::get_config` / `config_history`）：按 id 从新到旧
  遍历，跳过无法解析为目标类型的行；一行损坏不阻断更早的可用配置。
- **领域层**（bootstrap 的 `select_startup_model`）：解析之外还需领域校验
  （provider 已从 `[providers]` 删除、模型已不在 models.dev 目录中），
  沿回退链逐条尝试，第一条可完整解析的选择生效；链空或全部失效时落
  内置默认（环境推断 provider + 内置默认模型）。每次回退都告警，用户能
  看到哪条选择失效、回退到了哪。

### 启动解析优先级

```text
provider/model 选择：CLI 参数（--provider/--model，支持 <provider>/<模型id> 全形式）
                   → sqlite 配置回退链
                   → 内置默认（ANTHROPIC_API_KEY → anthropic，否则 openai）
其余配置项：       CLI 参数 → 环境变量 → 配置文件 → 内置默认（不变）
```

CLI 给出任一选择器时数据库选择整层不生效且**不写回**——CLI 是临时覆盖，
`/models` 命令才是改变持久选择的入口。配置文件中的 `provider` / `model`
选择器键移除（`deny_unknown_fields` 硬报错提示迁移）；`[providers]` 定义、
连接/请求参数、压缩阈值等仍在配置文件中，与 sqlite 配置共存，逐步迁移。

### 跨 provider 的 `/models`

- `ModelResolver` 从「绑定单一 provider」重构为持有全部 provider 的连接层
  输入，按 `<provider, 模型id>` 解析；选择器候选覆盖 内置 provider ∪
  `[providers]` 定义的 provider，行 id 为 `<provider>/<模型id>`。
- 运行时跨 provider 切换需要替换连接实现：`Agent::set_provider` 与
  `set_model` 配对调用，api_key 按启动同一口径重新分层（环境变量 >
  `providers.<名字>.api_key` > 平铺配置；CLI `--api-key` 属于启动 provider，
  不参与运行时切换分层）。
- 切换成功后把 `<provider>/<模型id>` 追加到 config 表；库不可用（启动已
  告警降级）时跳过，写失败只记日志不打断切换。

## Consequences

- `/models` 的选择对后续启动生效，选择即持久状态；选错或配置演进导致
  失效时自动回退，不会出现「配置文件改坏后起不来」的硬故障。
- config 表只增不删：磁盘占用随修改次数线性增长（行极小，可忽略）；
  如需清理可整表删除（等价于回到内置默认）。
- 配置文件仍是 provider 定义（base_url / api_key / 模型规格覆盖）的唯一
  来源；把它们也迁入 config 表需要配套的编辑命令（`/providers ...`），
  属于后续工作。
- `--model` 与 `/models:<p>/<id>` 按第一个 `/` 切分 provider 与模型 id，
  模型 id 自身含 `/`（如 openrouter 的 `openai/gpt-4o`）写作
  `<provider>/openai/gpt-4o`，无歧义。
