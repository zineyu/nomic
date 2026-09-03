-- 统一 Entry payload（ADR-0045）：条目种类由 role 列完整表达
-- （user / assistant / tool_result / compaction），kind 列随之删除。
-- payload 由旧双格式（Message / CompactionRecord JSON）重写为 Entry JSON
-- 的迁移在 Rust 侧执行（nomic-session `migration` 模块，PRAGMA
-- user_version 把关），不属本脚本。

ALTER TABLE entries DROP COLUMN kind;
