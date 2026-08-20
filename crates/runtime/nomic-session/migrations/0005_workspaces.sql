-- workspace 一等实体：一个 workspace 对应文件系统中的一个路径（规范化绝对路径，
-- 全局唯一）；session 创建时绑定 workspace，其所有操作以 workspace 路径为基准。

CREATE TABLE workspaces (
    id             TEXT PRIMARY KEY,        -- UUID（迁移行用随机 hex，新行用 UUID v7）
    path           TEXT NOT NULL UNIQUE,    -- workspace 路径（写入侧负责规范化）
    created_at     INTEGER NOT NULL,        -- 首次登记时间（Unix 毫秒）
    last_active_at INTEGER                  -- 最近活跃（session 创建 / 条目追加时推进）
);

ALTER TABLE sessions ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);

-- 既有数据迁移：每个 distinct cwd 登记一个 workspace（id 用随机 hex），
-- 回填 sessions.workspace_id；path 唯一约束天然去重并发写。
INSERT INTO workspaces (id, path, created_at, last_active_at)
SELECT lower(hex(randomblob(16))), cwd,
       COALESCE(MIN(first_message_at), 0), MAX(last_message_at)
FROM sessions GROUP BY cwd;

UPDATE sessions SET workspace_id =
    (SELECT id FROM workspaces WHERE workspaces.path = sessions.cwd);

-- cwd 的唯一持有者变为 workspace，删除冗余列避免双写漂移（SQLite >= 3.35）。
ALTER TABLE sessions DROP COLUMN cwd;
