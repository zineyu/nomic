-- nomic 存储 schema v1（旧版本 fixture）。
--
-- 本文件镜像 storage/schema.rs 中迁移 1 的 DDL，用于在测试中构造
-- 「旧版本 release 写出」的库：测试只能依赖 schema 的版本契约，
-- 不能依赖旧版本的 Rust 代码。v2 为后续迁移添加 sessions.state。

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    parent_session TEXT,
    forked_at TEXT,
    working_dir TEXT NOT NULL
);

CREATE TABLE messages (
    session_id TEXT NOT NULL REFERENCES sessions (id),
    ordinal INTEGER NOT NULL,
    message_id TEXT NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (session_id, ordinal)
);

CREATE TABLE steps (
    session_id TEXT NOT NULL REFERENCES sessions (id),
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    call_id TEXT,
    message_id TEXT,
    body TEXT NOT NULL,
    PRIMARY KEY (session_id, ordinal)
);

CREATE TABLE submissions (
    request_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL
);
