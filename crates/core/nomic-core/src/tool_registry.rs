//! 工具注册表：类型擦除条目按名称派发。
//!
//! `Tool` 的关联类型无法异构 dyn 化；注册表以「声明 + 装箱闭包」
//! 为条目按名称派发。闭包内完成调用参数 JSON → Input 反序列化与
//! Output → JSON 文本序列化，类型边界不外泄：反序列化失败归为
//! [`ToolError::InvalidInput`]，工具自身报错归为 [`ToolResult::Error`]，
//! 成功输出归为 [`ToolResult::Ok`]，三层语义与 [`Tool`] 一致。

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::tool::{Tool, ToolError, ToolName, ToolResult, ToolSpec};

/// 类型擦除的异步调用：入参为工作目录坐标与调用参数 JSON，
/// 出参为三层结果之一。
type ErasedCall = Box<
    dyn for<'w> Fn(&'w Path, Value) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'w>>
        + Send
        + Sync,
>;

/// 注册表条目：工具声明 + 类型擦除调用。
struct Entry {
    spec: ToolSpec,
    call: ErasedCall,
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
}

/// 工具注册表：名称 → 声明 + 类型擦除调用。
///
/// 工具实例经 [`ToolRegistry::register`] 登记；查询声明供 LLM 请求
/// 传入，派发经 [`ToolRegistry::call`] 按名称进行。
#[derive(Default)]
pub struct ToolRegistry {
    entries: Vec<Entry>,
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("specs", &self.specs())
            .finish_non_exhaustive()
    }
}

impl ToolRegistry {
    /// 创建空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记工具；重名属于注册代码错误，立即 panic 以 fast-fail。
    ///
    /// # Panics
    ///
    /// 同名工具已登记时 panic。
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        let spec = tool.spec();
        assert!(
            !self
                .entries
                .iter()
                .any(|entry| entry.spec.name == spec.name),
            "duplicate tool registration: {}",
            spec.name
        );
        let tool = Arc::new(tool);
        let call: ErasedCall = Box::new(move |working_dir: &Path, raw: Value| {
            let tool = Arc::clone(&tool);
            let invocation: Pin<Box<dyn Future<Output = ToolResult> + Send + '_>> =
                Box::pin(async move {
                    let input = match serde_json::from_value::<T::Input>(raw) {
                        Ok(input) => input,
                        Err(error) => {
                            return ToolResult::from(ToolError::InvalidInput(error.to_string()));
                        }
                    };
                    match tool.call(working_dir, input).await {
                        // 输出为 JSON 文本，与 output_schema 对应；
                        // 序列化失败是工具自身缺陷，归为报错层回灌模型
                        Ok(output) => match serde_json::to_string(&output) {
                            Ok(json) => ToolResult::ok(json),
                            Err(error) => ToolResult::from(ToolError::Failed(format!(
                                "tool output serialization failed: {error}"
                            ))),
                        },
                        Err(error) => ToolResult::from(error),
                    }
                });
            invocation
        });
        self.entries.push(Entry { spec, call });
    }

    /// 全部工具声明（随 LLM 请求传入）。
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.entries
            .iter()
            .map(|entry| entry.spec.clone())
            .collect()
    }

    /// 按名称查询工具声明。
    #[must_use]
    pub fn spec(&self, name: &ToolName) -> Option<&ToolSpec> {
        self.entries
            .iter()
            .map(|entry| &entry.spec)
            .find(|spec| &spec.name == name)
    }

    /// 按名称派发调用：未注册名称显式失败（[`UnknownTool`]），
    /// 返回的 future 为工具执行本身（只借用工作目录坐标）。
    ///
    /// # Errors
    ///
    /// 名称未注册时返回 [`UnknownTool`]。
    pub fn call<'w>(
        &self,
        name: &ToolName,
        working_dir: &'w Path,
        raw: Value,
    ) -> Result<Pin<Box<dyn Future<Output = ToolResult> + Send + 'w>>, UnknownTool> {
        let entry = self
            .entries
            .iter()
            .find(|entry| &entry.spec.name == name)
            .ok_or_else(|| UnknownTool(name.clone()))?;
        Ok((entry.call)(working_dir, raw))
    }
}

/// 派发前置条件不满足：请求了未注册的工具。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown tool: {0}")]
pub struct UnknownTool(pub ToolName);

#[cfg(test)]
mod tests {
    use std::path::Path;

    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::tool::Tool;

    #[derive(Debug, Deserialize, JsonSchema)]
    struct UpperInput {
        text: String,
    }

    #[derive(Debug, Serialize, JsonSchema, PartialEq)]
    struct UpperOutput {
        text: String,
    }

    #[derive(Debug, Clone, Copy)]
    struct UpperTool;

    impl Tool for UpperTool {
        type Input = UpperInput;
        type Output = UpperOutput;

        fn name(&self) -> ToolName {
            ToolName::new("upper")
        }

        fn description(&self) -> &'static str {
            "转大写"
        }

        fn replay_safe(&self) -> bool {
            true
        }

        async fn call(
            &self,
            _working_dir: &Path,
            input: Self::Input,
        ) -> Result<Self::Output, ToolError> {
            Ok(UpperOutput {
                text: input.text.to_uppercase(),
            })
        }
    }

    fn registry_with_upper() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(UpperTool);
        registry
    }

    #[test]
    fn 声明随登记可见且名称唯一() {
        let registry = registry_with_upper();
        let specs = registry.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name.as_str(), "upper");
        assert!(specs[0].replay_safe);
        assert!(registry.spec(&ToolName::new("upper")).is_some());
        assert!(registry.spec(&ToolName::new("nope")).is_none());
    }

    #[test]
    #[should_panic(expected = "duplicate tool registration")]
    fn 重名登记_fast_fail() {
        let mut registry = registry_with_upper();
        registry.register(UpperTool);
    }

    #[test]
    fn 未注册名称_显式失败类别() {
        let registry = registry_with_upper();
        let error = registry
            .call(
                &ToolName::new("nope"),
                Path::new("/"),
                serde_json::json!({}),
            )
            .err()
            .expect("未注册名称必须显式失败");
        assert_eq!(error, UnknownTool(ToolName::new("nope")));
        assert!(error.to_string().contains("nope"));
    }

    #[test]
    fn 参数反序列化失败归为_invalid_input_回灌为_error_层() {
        let registry = registry_with_upper();
        let outcome = futures::executor::block_on(
            registry
                .call(
                    &ToolName::new("upper"),
                    Path::new("/"),
                    serde_json::json!({"text": 42}),
                )
                .unwrap(),
        );
        assert!(
            matches!(outcome, ToolResult::Error { ref message } if message.contains("invalid input")),
            "实际结果: {outcome:?}"
        );
    }

    #[test]
    fn 成功输出为_json_文本且工具报错归_error_层() {
        let registry = registry_with_upper();
        let ok = futures::executor::block_on(
            registry
                .call(
                    &ToolName::new("upper"),
                    Path::new("/"),
                    serde_json::json!({"text": "nomic"}),
                )
                .unwrap(),
        );
        match ok {
            ToolResult::Ok { output } => {
                let parsed: Value = serde_json::from_str(&output).unwrap();
                assert_eq!(parsed["text"], serde_json::json!("NOMIC"));
            }
            other => panic!("应为成功层: {other:?}"),
        }
    }

    #[test]
    fn 输出序列化失败归为_error_层回灌() {
        /// 自定义 Serialize 失败：契约不保证序列化成功。
        #[derive(JsonSchema)]
        struct Unserializable;

        impl Serialize for Unserializable {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("boom"))
            }
        }

        struct FragileTool;

        impl Tool for FragileTool {
            type Input = UpperInput;
            type Output = Unserializable;

            fn name(&self) -> ToolName {
                ToolName::new("fragile")
            }

            fn description(&self) -> &'static str {
                "输出序列化必失败"
            }

            fn replay_safe(&self) -> bool {
                true
            }

            async fn call(
                &self,
                _working_dir: &Path,
                _input: Self::Input,
            ) -> Result<Self::Output, ToolError> {
                Ok(Unserializable)
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(FragileTool);
        let outcome = futures::executor::block_on(
            registry
                .call(
                    &ToolName::new("fragile"),
                    Path::new("/"),
                    serde_json::json!({"text": "x"}),
                )
                .unwrap(),
        );
        assert!(
            matches!(outcome, ToolResult::Error { ref message } if message.contains("output serialization failed")),
            "实际结果: {outcome:?}"
        );
    }

    #[test]
    fn 工作目录坐标透传给工具() {
        struct ProbeTool;

        impl Tool for ProbeTool {
            type Input = UpperInput;
            type Output = UpperOutput;

            fn name(&self) -> ToolName {
                ToolName::new("probe")
            }

            fn description(&self) -> &'static str {
                "透传工作目录"
            }

            fn replay_safe(&self) -> bool {
                true
            }

            async fn call(
                &self,
                working_dir: &Path,
                input: Self::Input,
            ) -> Result<Self::Output, ToolError> {
                Ok(UpperOutput {
                    text: format!("{}:{}", working_dir.display(), input.text),
                })
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(ProbeTool);
        let root = Path::new("/tmp/workspace");
        let outcome = futures::executor::block_on(
            registry
                .call(
                    &ToolName::new("probe"),
                    root,
                    serde_json::json!({"text": "x"}),
                )
                .unwrap(),
        );
        match outcome {
            ToolResult::Ok { output } => assert!(output.contains("/tmp/workspace")),
            other => panic!("应为成功层: {other:?}"),
        }
    }
}
