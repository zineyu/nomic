//! Kimi provider：Moonshot / Kimi For Coding 端点（`api.kimi.com/coding`）。
//!
//! 协议层是 OpenAI Chat Completions，但 Moonshot 服务端用 walle 校验器把
//! `tools.function.parameters` 限制在 MFJS（Moonshot Flavored JSON Schema）
//! 子集内：schemars 原生输出中的 `oneOf`、多类型 `type` 数组、`$schema` /
//! `title` / `format` 注解都会被 400 拒绝（`oneOf` 挂在 `$ref` 后还会被
//! 误判为「无限递归」）。因此本 provider 复用 [`OpenAiProvider`] 的传输
//! 实现，但固定使用 [`ToolSchemaDialect::Mfjs`] 方言发送工具 schema。

use tokio_util::sync::CancellationToken;

use super::openai::schema::ToolSchemaDialect;
use super::openai::{OpenAiCompat, OpenAiProvider};
use crate::stream::{AssistantStream, Provider, StreamOptions};
use crate::types::{Context, Model};

/// Kimi provider：OpenAI Completions 传输 + MFJS 工具 schema 方言。
pub struct KimiProvider(OpenAiProvider);

impl KimiProvider {
    /// 创建 provider；`api_key` 为 `None` 时每次请求回退到 `KIMI_API_KEY`
    /// 环境变量（[`StreamOptions::api_key`] 优先，与 OpenAI provider 同口径）。
    pub fn new(api_key: Option<String>) -> Self {
        Self(OpenAiProvider::new(api_key, kimi_compat()))
    }
}

/// Kimi 端点的兼容修补：固定 MFJS 工具 schema 方言，其余与 OpenAI 默认一致。
fn kimi_compat() -> OpenAiCompat {
    OpenAiCompat {
        tool_schema_dialect: ToolSchemaDialect::Mfjs,
        api_key_env: Some("KIMI_API_KEY"),
        ..OpenAiCompat::default()
    }
}

impl std::fmt::Debug for KimiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KimiProvider").finish_non_exhaustive()
    }
}

impl Provider for KimiProvider {
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: &StreamOptions,
        cancel: CancellationToken,
    ) -> AssistantStream {
        self.0.stream(model, context, options, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kimi_provider_uses_mfjs_tool_schema_dialect() {
        // 与 openai::tests::mfjs_dialect_normalizes_tool_parameters 组合成完整
        // 链路：KimiProvider 固定 Mfjs 方言 + build_request 见到 Mfjs 方言时
        // 把工具 schema 归一化为 MFJS 子集
        assert_eq!(kimi_compat().tool_schema_dialect, ToolSchemaDialect::Mfjs);
        assert_eq!(kimi_compat().api_key_env, Some("KIMI_API_KEY"));
    }
}
