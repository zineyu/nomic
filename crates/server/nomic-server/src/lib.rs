//! nomic 常驻 server。
//!
//! 会话执行逻辑为调度重驱状态机：受理把会话排入待处理队列，
//! worker 领取会话并串行执行 agent loop 到静止后释放。
//! SQLite 存储实现见 [`storage`]；单轮对话执行引擎见 [`agent`]；
//! IPC 服务端见 [`ipc`]；协议客户端见 [`client`]；server 生命周期
//! （自动拉起/停止）见 [`daemon`]；启动恢复见 [`recovery`]。

pub mod agent;
pub mod client;
pub mod config;
pub mod daemon;
pub mod environment;
pub mod ipc;
pub mod recovery;
pub mod storage;
pub mod tools;

pub use environment::LocalEnvironment;
