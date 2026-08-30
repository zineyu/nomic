# ADR-0039: 设置全部存储于 SQLite（移除 config.toml）

- 状态：已接受
- 日期：2026-08-30

## 背景

provider/model 的**选择**早已迁入 sqlite `config` 表（`/models` 命令），
但 provider 定义（`[providers]` 连接参数与模型规格覆盖）、请求参数
（`temperature` / `max_tokens` / `append_system` 等）、`[compaction]`、
`[model_aliases]`、`prompts` 仍在 `config.toml`。`config.rs` 的模块文档
当时就声明二者「逐步向 sqlite 迁移期间暂时共存」——本 ADR 完成这次迁移。

双数据源的代价已经显现：

1. **两个写入面**：TUI `/models` 写 sqlite，其余设置只能手工编辑 TOML，
   web 模式（多 session、前端驱动）下文件配置尤其别扭——前端无法读写它；
2. **运行时不一致**：运行中的进程对文件配置的修改无感知（启动时读一次），
   而 sqlite 配置天然支持运行时写入；
3. **校验链分裂**：拼写防呆（`deny_unknown_fields`）只覆盖文件层。

## 决策

**不再读取和使用配置文件；所有设置存储在 sqlite。** `config.toml` 被完全
忽略：不读取、不迁移、不报错；`config.example.toml` 删除，用户经
`nomic config` 入口自行建立配置。

### 存储模型（migration 0008，三张新表）

```sql
-- provider 定义（替代 config.toml [providers]；列式，可逐字段更新）
CREATE TABLE providers (
    name       TEXT PRIMARY KEY,
    api        TEXT,     -- anthropic_messages / open_ai_completions；NULL = 按名推断
    base_url   TEXT,
    api_key    TEXT,
    updated_at INTEGER NOT NULL
) STRICT;

-- 模型规格覆盖（替代 providers.<名>.models.<id>；NULL = 未覆盖，
-- 向下回退 models.dev / 中性兜底）
CREATE TABLE model_specs (
    provider         TEXT NOT NULL REFERENCES providers(name) ON DELETE CASCADE,
    model_id         TEXT NOT NULL,
    name             TEXT,
    reasoning        INTEGER,
    vision           INTEGER,
    context_window   INTEGER,
    max_tokens       INTEGER,
    cost_input       REAL,
    cost_output      REAL,
    cost_cache_read  REAL,
    cost_cache_write REAL,
    updated_at       INTEGER NOT NULL,
    PRIMARY KEY (provider, model_id)
) STRICT;

-- 标量设置（upsert 语义；temperature / max_tokens / append_system /
-- base_url / api_key 兜底 / compaction.* / prompts / model_aliases）
CREATE TABLE settings (
    "key"      TEXT PRIMARY KEY,
    value      ANY NOT NULL,   -- jsonb() 写入；读取用 json(value) 解码
    updated_at INTEGER NOT NULL
) STRICT;
```

- 与 append-only 的 `config` 表不同，三张新表都是**当前值**语义
  （upsert / delete）：设置类数据的「历史回退」没有意义，set/unset 直观；
- 现有 `config` 表保留不动：模型选择与思考级别的回退链（feedback）
  是有意设计（失效选择逐条回退），不属于本次迁移范围；
- 删除 provider 级联删除其 `model_specs` 行。

### 分层口径（config.toml 层原位替换为 sqlite）

| 字段 | 分层（高 → 低） |
| --- | --- |
| base_url | CLI > env（OPENAI_BASE_URL，仅 openai 系）> `providers.base_url` > `settings.base_url` > 协议默认 |
| api_key | CLI > env（ANTHROPIC_API_KEY / OPENAI_API_KEY）> `providers.api_key` > `settings.api_key` |
| 模型规格字段 | `model_specs` 行 > models.dev > 中性兜底 |
| temperature / max_tokens / append_system | CLI > `settings` |
| compaction（enabled/reserve/keep_recent） | `settings` > 内置默认 |
| model_aliases / prompts 显式路径 | `settings`（prompts 仍叠加 CLI `--prompt-template`） |
| provider/model 选择、reasoning | 不变（CLI > `config` 表回退链） |

### Settings 快照与 reload

`ModelResolver` 原持有的 `Option<Config>` 换成 `RwLock<Settings>` 快照
（providers + model_specs + 标量一次读出）。bootstrap 从 store 加载；
TUI `/config` 与 web REST 的写操作在落库后调用 `resolver.reload()` 使
运行进程立即生效。跨进程 staleness 与文件时代一致（启动读一次），不引入
新的监听机制。

### 写入入口（三端共用一套核心逻辑）

1. **CLI**：`nomic config` 子命令族——
   `providers list/set/unset`、`models list/set/unset <provider>/<模型id>`
   （逐字段 flag，只更新显式传入的字段）、`set/get/unset/list <key>`；
2. **TUI**：`/config ...` slash 命令，复用同一 clap 解析（
   `/config set temperature 0.7`），结果作为系统消息显示，写后 reload；
3. **Web**：复用已有 WebSocket 事件协议（不新增 REST）——查询事件
   `get_settings`（响应 `SettingsSnapshot`：providers + model_specs +
   标量全量）；查询式命令 `upsert_provider` / `delete_provider` /
   `upsert_model_spec` / `delete_model_spec` / `set_setting` /
   `unset_setting`（携带 `request_id`，ack 或 error 带同一 id，同
   `create_session` 先例）；写后 reload 并广播无 session 维度的
   `settings_changed`（同 `Refresh` 先例），其他客户端收到后重新拉取
   快照、模型候选随之刷新；React 设置页（providers / 模型规格 / 标量
   设置的可视化管理）经同一 WS 连接读写。

web 模式各 session 共享进程级 `Arc<ModelResolver>`，reload 对全部 session
生效（连接参数本来就是进程级语义）。

## 非目标

- prompt templates（`.md` 发现）、skills、AGENTS.md 仍是文件——它们是
  被发现的内容，不是设置；`prompts` 设置项仅保存显式追加的路径列表；
- `config` append-only 表及其回退链语义不变；
- 环境变量与 CLI 参数层保留（优先级不变）；
- 旧库不做数据迁移（`config.toml` 从未入库，无历史包袱）。

## 影响

- `nomic-cli/src/config.rs` 重写为设置类型与校验（删除文件加载、
  `Config` / `ProviderConfig` / `CompactionConfig`）；`config.example.toml`
  及其 schema 同步测试删除；
- ADR-0001（配置优先级表述）、ADR-0010、ADR-0038（`[model_aliases]` 出处）
  中涉及 `config.toml` 的表述由本 ADR 取代（历史 ADR 原文不改）；
- README 配置章节重写为 `nomic config` 用法；CHANGELOG 记录破坏性变更
  （`BREAKING CHANGE:` 用户的 config.toml 不再生效，需用 `nomic config`
  重建）。
