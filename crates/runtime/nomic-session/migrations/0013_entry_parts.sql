-- entry 内容块规范化（ADR-0045 Amendments）：parts 独立成表，entries
-- 不再以 JSON 存 parts；assistant 响应元数据移至 entries.meta。
--
-- parts 主键 (entry_id, seq, sub_seq)：seq 为顶层块序；tool_result 的
-- 嵌套内容块（text/image）以 sub_seq > 0 挂在其所属块（sub_seq = 0）下。
-- 存量数据由 json_each 一次性拆解（本脚本运行前 Rust 侧已把旧
-- Message/CompactionRecord payload 统一为 Entry JSON，见 migration 模块）；
-- 损坏 payload（json_valid = 0）的行跳过——其数据本就无法加载。

CREATE TABLE parts (
    entry_id      TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    seq           INTEGER NOT NULL,
    sub_seq       INTEGER NOT NULL DEFAULT 0,
    type          TEXT NOT NULL,
    text          TEXT,
    signature     TEXT,
    redacted      INTEGER NOT NULL DEFAULT 0,
    data          TEXT,
    mime_type     TEXT,
    tool_call_id  TEXT,
    tool_name     TEXT,
    arguments     TEXT,
    details       TEXT,
    is_error      INTEGER NOT NULL DEFAULT 0,
    summary       TEXT,
    kept_count    INTEGER,
    tokens_before INTEGER,
    PRIMARY KEY (entry_id, seq, sub_seq)
) STRICT;

ALTER TABLE entries ADD COLUMN meta TEXT;

-- 顶层块拆解（signature 列按 type 复用 text/thinking/thought 三种签名键；
-- text 列复用 text/thinking 两种文本键）
INSERT INTO parts (entry_id, seq, sub_seq, type, text, signature, redacted,
                   data, mime_type, tool_call_id, tool_name, arguments, details,
                   is_error, summary, kept_count, tokens_before)
SELECT e.id, p.key, 0,
       p.value ->> '$.type',
       COALESCE(p.value ->> '$.text', p.value ->> '$.thinking'),
       COALESCE(p.value ->> '$.text_signature', p.value ->> '$.thinking_signature',
                p.value ->> '$.thought_signature'),
       COALESCE(p.value ->> '$.redacted', 0),
       p.value ->> '$.data',
       p.value ->> '$.mime_type',
       COALESCE(p.value ->> '$.tool_call_id', p.value ->> '$.id'),
       COALESCE(p.value ->> '$.tool_name', p.value ->> '$.name'),
       json_extract(p.value, '$.arguments'),
       json_extract(p.value, '$.details'),
       COALESCE(p.value ->> '$.is_error', 0),
       p.value ->> '$.summary',
       p.value ->> '$.kept_count',
       p.value ->> '$.tokens_before'
FROM entries e, json_each(e.payload, '$.parts') p
WHERE json_valid(e.payload);

-- tool_result 的嵌套内容块（text/image）
INSERT INTO parts (entry_id, seq, sub_seq, type, text, data, mime_type)
SELECT e.id, p.key, c.key + 1,
       c.value ->> '$.type',
       c.value ->> '$.text',
       c.value ->> '$.data',
       c.value ->> '$.mime_type'
FROM entries e, json_each(e.payload, '$.parts') p, json_each(p.value, '$.content') c
WHERE json_valid(e.payload) AND p.value ->> '$.type' = 'tool_result';

-- assistant 响应元数据移至 meta 列
UPDATE entries SET meta = json_extract(payload, '$.response')
WHERE json_valid(payload) AND json_extract(payload, '$.response') IS NOT NULL;

ALTER TABLE entries DROP COLUMN payload;
