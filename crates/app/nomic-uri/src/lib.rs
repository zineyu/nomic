//! `nomic-uri`：内部 URI 路由系统（ADR-0040）。
//!
//! 把「agent 可寻址的非普通文件资源」统一抽象为 `scheme://` 内部 URI。
//! 模块随实施计划（docs/adr/0040-implementation-plan.md）逐步填充：
//! T1 解析器 → T2 router/trait → T3 选择器 → T4+ 协议 handler。

pub mod handler;
pub mod parse;
mod selector;

pub use handler::UriError;
pub use parse::{InternalUri, extract_uri_scheme, parse_internal_uri};
