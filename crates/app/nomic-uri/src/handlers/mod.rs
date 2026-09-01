//! 内置协议 handler。随实施计划逐个填充：skill（T4）、local（T9）、
//! artifact（T10）、history/agent（T11）。

pub mod local;
pub mod skill;

pub use local::LocalProtocolHandler;
pub use skill::SkillProtocolHandler;
