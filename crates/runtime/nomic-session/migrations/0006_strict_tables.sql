-- no-transaction
-- 全部业务表重建为 STRICT 表：列类型收敛到 STRICT 允许集
-- （INT/INTEGER/REAL/TEXT/BLOB/ANY），写入侧杜绝脏类型入库。
-- config.value 原为 JSONB 亲和列（不在 STRICT 允许集内），声明为 ANY：
-- jsonb() 写入的 BLOB 原样保留，读取侧 json(value) 解码不变。
--
-- 重建沿用 SQLite 官方表重建流程：foreign_keys 只能在事务外切换
-- （事务内 PRAGMA foreign_keys 是 no-op），故本迁移声明 no-transaction，
-- 事务由脚本自管。旧库数据自建库起即有外键约束保护（连接级 ON +
-- 编译期 SQLITE_DEFAULT_FOREIGN_KEYS=1），拷贝不会产生悬挂引用。
--
-- 顺序：先父表后子表，保证子表新建时 FK 引用的已是重建后的父表。

PRAGMA foreign_keys = OFF;

BEGIN IMMEDIATE;

CREATE TABLE new_workspaces (
    id             TEXT PRIMARY KEY,
    path           TEXT NOT NULL UNIQUE,
    created_at     INTEGER NOT NULL,
    last_active_at INTEGER
) STRICT;
INSERT INTO new_workspaces SELECT * FROM workspaces;
DROP TABLE workspaces;
ALTER TABLE new_workspaces RENAME TO workspaces;

CREATE TABLE new_sessions (
    id               TEXT PRIMARY KEY,
    first_message_at INTEGER,
    last_message_at  INTEGER,
    workspace_id     TEXT REFERENCES workspaces(id)
) STRICT;
INSERT INTO new_sessions SELECT * FROM sessions;
DROP TABLE sessions;
ALTER TABLE new_sessions RENAME TO sessions;

CREATE TABLE new_entries (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    parent_id  TEXT REFERENCES entries(id),
    role       TEXT NOT NULL,
    timestamp  INTEGER NOT NULL,
    payload    TEXT NOT NULL,
    kind       TEXT NOT NULL DEFAULT 'message'
) STRICT;
INSERT INTO new_entries SELECT * FROM entries;
DROP TABLE entries;
ALTER TABLE new_entries RENAME TO entries;
CREATE INDEX idx_entries_session ON entries(session_id);
CREATE INDEX idx_entries_parent ON entries(parent_id);

CREATE TABLE new_config (
    id         INTEGER PRIMARY KEY,
    "key"      TEXT NOT NULL,
    value      ANY NOT NULL,            -- jsonb() 写入的 BLOB；读取用 json(value) 解码
    updated_at INTEGER NOT NULL,
    session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE
) STRICT;
INSERT INTO new_config SELECT * FROM config;
DROP TABLE config;
ALTER TABLE new_config RENAME TO config;
CREATE INDEX idx_config_key ON config("key", id DESC);
CREATE INDEX idx_config_session_key ON config(session_id, "key", id DESC);
CREATE INDEX idx_config_global_key ON config("key", id DESC) WHERE session_id IS NULL;

COMMIT;

PRAGMA foreign_keys = ON;
