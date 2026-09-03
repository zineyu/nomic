-- sessions 外键子列索引（ADR-0044 归属链 project 1—N work 1—N session）。
--
-- 0010 为 sessions 新增 work_id / parent_session_id 外键但未建索引，
-- 所有按 work 汇聚的查询（work 列表摘要、main_session_of_work、
-- delete_work 级联、空壳清理）都退化为 sessions 全表扫描；
-- work 列表摘要的逐行相关子查询叠加后呈 O(works × sessions) 放大。
-- (work_id, parent_session_id) 同时覆盖"work 全部 session"与
-- "work 的主 session（parent_session_id IS NULL）"两类口径。

CREATE INDEX idx_sessions_work ON sessions(work_id, parent_session_id);
