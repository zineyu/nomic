//! Core domain primitives for the nomic workspace.
//!
//! 这是 workspace 的示例 crate，用于验证工具链、lint 与 CI 配置。

/// Returns a greeting addressed to `name`.
///
/// # Examples
///
/// ```
/// assert_eq!(nomic_core::greet("world"), "Hello, world!");
/// ```
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greets_by_name() {
        assert_eq!(greet("nomic"), "Hello, nomic!");
    }
}
