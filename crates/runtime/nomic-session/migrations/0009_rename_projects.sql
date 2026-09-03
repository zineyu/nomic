-- workspace 更名为 project（ADR-0044）：一个 project 对应一个 git repo。
-- 纯改名迁移，数据不动：SQLite >= 3.25 的 ALTER TABLE RENAME 会自动
-- 更新子表的外键引用（sessions.workspace_id → projects(id) 随表名改写）；
-- RENAME COLUMN 同步改写 sessions 内的 FK 定义。STRICT 表兼容两者。

ALTER TABLE workspaces RENAME TO projects;
ALTER TABLE sessions RENAME COLUMN workspace_id TO project_id;
