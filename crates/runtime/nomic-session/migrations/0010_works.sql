-- no-transaction
-- work 一等实体（ADR-0044）：一次 work 是程序的一次完整任务协作过程，
-- 可含多个 session（多 agent 协作）；归属链 project 1—N work 1—N session，
-- 均 NOT NULL。session 不再直挂 project（经 work 间接归属），原
-- sessions.project_id 列移除。
--
-- 既有数据回填：每个既有 session 生成一个 1:1 的 work（id 为
-- 'w-' + session id，确定性可关联），title/时间戳取自 session。
--
-- sessions 换外键列（去 project_id、加 work_id NOT NULL 与
-- parent_session_id 血缘列）需重建表，沿用 0006 的官方表重建流程：
-- foreign_keys 事务外切换，故声明 no-transaction，事务由脚本自管。

PRAGMA foreign_keys = OFF;

BEGIN IMMEDIATE;

CREATE TABLE works (
    id             TEXT PRIMARY KEY,        -- UUID v7（回填行用 'w-' + session id）
    project_id     TEXT NOT NULL REFERENCES projects(id),
    title          TEXT,                    -- NULL = 派生（主 session 标题）
    created_at     INTEGER NOT NULL,
    last_active_at INTEGER                  -- session 创建 / 条目追加时推进
) STRICT;

-- 回填：每个既有 session 一个 1:1 work
INSERT INTO works (id, project_id, title, created_at, last_active_at)
SELECT 'w-' || id, project_id, title,
       COALESCE(first_message_at, 0), last_message_at
FROM sessions;

CREATE TABLE new_sessions (
    id                TEXT PRIMARY KEY,
    first_message_at  INTEGER,
    last_message_at   INTEGER,
    title             TEXT,
    work_id           TEXT NOT NULL REFERENCES works(id) ON DELETE CASCADE,
    parent_session_id TEXT REFERENCES sessions(id)  -- NULL = 主 session；子 agent session 记父血缘
) STRICT;

INSERT INTO new_sessions (id, first_message_at, last_message_at, title, work_id, parent_session_id)
SELECT id, first_message_at, last_message_at, title, 'w-' || id, NULL FROM sessions;

DROP TABLE sessions;
ALTER TABLE new_sessions RENAME TO sessions;

COMMIT;

PRAGMA foreign_keys = ON;
