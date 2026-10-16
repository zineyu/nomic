//! nomic 常驻 server。
//!
//! 会话执行逻辑为调度重驱状态机：受理把会话排入待处理队列，
//! worker 领取会话并串行执行 agent loop 到静止后释放。
//! SQLite 存储实现见 [`storage`]；单轮对话执行引擎见 [`agent`]；
//! 调度器与 IPC 服务端在后续切片落地。

pub mod agent;
pub mod storage;
