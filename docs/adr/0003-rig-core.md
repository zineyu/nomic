# ADR-0003: LLM 客户端选型 rig-core，薄封装隔离

## Status

Accepted

## Context

coding agent 需要 tokio 异步、SSE 流式输出、function/tool calling，
并希望覆盖多家 provider（OpenAI 兼容端点 + Anthropic/Gemini 原生等）。
候选：rig-core、genai、async-openai（mistral.rs 是推理引擎、ollama-rs
单 provider，均出局）。

核实事实（crates.io，2026-10）：

| crate | 版本 | 许可证 | MSRV | 90 天下载量 |
|---|---|---|---|---|
| rig-core | 0.44.0 | MIT | 1.95 | ~190 万 |
| async-openai | 0.42.2 | MIT | 1.75 | ~290 万（仅 OpenAI 兼容协议） |
| genai | 0.7.0-rc.7 | MIT/Apache | — | ~22 万（tool calling 较新） |

## Decision

采用 **rig-core**，项目内通过适配器（薄封装）间接使用，只使用其
completion/agent 能力，不引入 embeddings/vector store 等无关模块。
锁定精确版本管理，升级时预期有迁移工作。

## Consequences

- 免费获得 20+ provider（含 OpenAI 兼容自定义端点，覆盖
  DeepSeek/vLLM/Ollama 等）。
- 核心代码不直接依赖 rig 类型，未来可替换。
- 风险：pre-1.0 破坏性升级；部分 provider 集成质量参差，需在 CI 用
  集成测试验证所用 provider 的 streaming + tools。
