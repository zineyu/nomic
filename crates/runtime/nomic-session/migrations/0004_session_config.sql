-- 配置表支持按 session 隔离：session_id 为 NULL 的行是全局配置
-- （启动默认 / 回退链），非 NULL 的行是会话级覆盖（web 多 session 各自的
-- 模型 / 思考级别选择）。两类行用不同索引分开读取，避免混读。

ALTER TABLE config ADD COLUMN session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE;

-- 会话级读取：按 (session_id, key) 回退链（最新在前）
CREATE INDEX idx_config_session_key ON config(session_id, "key", id DESC);
-- 全局读取：仅 session_id IS NULL 的行（排除会话级覆盖行）
CREATE INDEX idx_config_global_key ON config("key", id DESC) WHERE session_id IS NULL;
