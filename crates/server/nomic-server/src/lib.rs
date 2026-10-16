//! nomic 常驻 server。
//!
//! 由受理（requestId 去重）、调度器（并发上限、排队）、worker 池组成；
//! 会话执行逻辑为调度重驱状态机。SQLite 存储实现已落地
//!（见 `storage`）；受理、调度器与 worker 池在后续切片落地。

pub mod storage;
