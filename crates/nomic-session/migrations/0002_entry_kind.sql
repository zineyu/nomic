-- entries 区分条目种类：message（对话消息）/ compaction（压缩条目）。
-- compaction 条目的 payload 是 CompactionRecord JSON 而非 Message JSON，
-- load_messages 依此重建「摘要 + 保留尾部」的有效上下文。

ALTER TABLE entries ADD COLUMN kind TEXT NOT NULL DEFAULT 'message';
