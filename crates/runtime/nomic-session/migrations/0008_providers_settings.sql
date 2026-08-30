-- 设置全部存储于 sqlite（ADR-0039，替代 config.toml）：provider 定义、
-- 模型规格覆盖、标量设置三张「当前值」语义的新表（upsert / delete），
-- 区别于 append-only 的 config 表（模型选择 / 思考级别的回退链不变）。

-- provider 定义（替代 config.toml [providers]）：api 为 NULL 时按名推断
-- （anthropic / openai），其余字段 NULL = 未设置（向下回退环境变量 /
-- 协议默认）。
CREATE TABLE providers (
    name       TEXT PRIMARY KEY,
    api        TEXT,
    base_url   TEXT,
    api_key    TEXT,
    updated_at INTEGER NOT NULL
) STRICT;

-- 模型规格覆盖（替代 providers.<名>.models.<模型id>）：字段 NULL = 未覆盖，
-- 向下回退 models.dev / 中性兜底；provider 删除时级联清除。
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

-- 标量设置（替代 config.toml 平铺键 / [compaction] / [model_aliases] /
-- prompts）：upsert 语义，unset 即删除行；值用 sqlite 原生 JSON 类型存储
-- （同 config 表口径：jsonb() 写入，json(value) 解码）。
CREATE TABLE settings (
    "key"      TEXT PRIMARY KEY,
    value      ANY NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
