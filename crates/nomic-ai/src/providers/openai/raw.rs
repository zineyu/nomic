//! OpenAI 线上协议的反序列化类型。

use serde::Deserialize;

// ── 线上协议的反序列化类型 ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(super) struct RawChunk {
    pub(super) id: Option<String>,
    pub(super) model: Option<String>,
    pub(super) choices: Vec<RawChoice>,
    pub(super) usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawChoice {
    pub(super) delta: Option<RawDelta>,
    pub(super) finish_reason: Option<String>,
    pub(super) usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawDelta {
    pub(super) content: Option<String>,
    pub(super) reasoning_content: Option<String>,
    pub(super) reasoning: Option<String>,
    pub(super) reasoning_text: Option<String>,
    pub(super) tool_calls: Option<Vec<RawToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawToolCallDelta {
    pub(super) index: Option<usize>,
    pub(super) id: Option<String>,
    pub(super) function: Option<RawFunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawFunctionDelta {
    pub(super) name: Option<String>,
    pub(super) arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawUsage {
    pub(super) prompt_tokens: Option<u64>,
    pub(super) completion_tokens: Option<u64>,
    pub(super) total_tokens: Option<u64>,
    pub(super) prompt_tokens_details: Option<RawPromptTokensDetails>,
    pub(super) completion_tokens_details: Option<RawCompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawPromptTokensDetails {
    pub(super) cached_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawCompletionTokensDetails {
    pub(super) reasoning_tokens: Option<u64>,
}
