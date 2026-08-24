-- 会话自定义标题（rename_session）：NULL = 派生标题（首条 user 消息摘要）。

ALTER TABLE sessions ADD COLUMN title TEXT;
