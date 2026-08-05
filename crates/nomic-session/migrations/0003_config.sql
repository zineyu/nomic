-- 配置表：append-only 的配置历史。每次修改配置新增一行（不回写旧行），
-- 读取方从最新一行向最老一行逐步回退（feedback），直到没有可回退的行为止。
-- value 用 sqlite 原生 JSON 类型存储（JSONB 二进制格式，需 SQLite >= 3.45）。

CREATE TABLE config (
    id         INTEGER PRIMARY KEY,
    "key"      TEXT NOT NULL,       -- 配置键（如 "model"）
    value      JSONB NOT NULL,      -- 配置值（sqlite 原生 JSON）
    updated_at INTEGER NOT NULL     -- 更新时间戳（Unix 毫秒）
);
CREATE INDEX idx_config_key ON config("key", id DESC);
