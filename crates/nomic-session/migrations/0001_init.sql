-- session 与消息条目（entries 按 parent_id 组织为树，顺序会话是树的特例）。
-- 时间均为 Unix 毫秒时间戳，直接复用消息自带的 timestamp。

CREATE TABLE sessions (
    id               TEXT PRIMARY KEY,          -- UUID v7
    cwd              TEXT NOT NULL,             -- 启动位置（工作目录）
    first_message_at INTEGER,                   -- 首条消息时间，无消息时 NULL
    last_message_at  INTEGER                    -- 末条消息时间，无消息时 NULL
);

CREATE TABLE entries (
    id         TEXT PRIMARY KEY,                -- 每条消息的 entry id（UUID v7）
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    parent_id  TEXT REFERENCES entries(id),     -- NULL = 根；分支 = 同一 parent 多个子节点
    role       TEXT NOT NULL,                   -- user / assistant / tool_result（查询用冗余列）
    timestamp  INTEGER NOT NULL,                -- 消息自带的 Unix 毫秒时间戳
    payload    TEXT NOT NULL                    -- nomic_ai::Message 的完整 serde JSON
);
CREATE INDEX idx_entries_session ON entries(session_id);
CREATE INDEX idx_entries_parent ON entries(parent_id);
