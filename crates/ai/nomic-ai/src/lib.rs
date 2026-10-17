//! nomic 的 rig-core LLM 适配层。
//!
//! 全仓库唯一允许出现 rig 类型的 crate。对外暴露
//! [`nomic_core::LlmAdapter`] 的实现 [`RigAdapter`] 与 provider 配置。

#[cfg(any(test, feature = "testing"))]
mod sse;

mod adapter;
mod config;

pub use adapter::RigAdapter;
pub use config::{ApiKey, ProviderConfig, ProviderConfigError};

#[cfg(feature = "testing")]
pub mod testing;
