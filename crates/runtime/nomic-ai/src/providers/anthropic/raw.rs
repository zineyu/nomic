//! Anthropic 线上协议的反序列化类型。

use serde::Deserialize;

// ── 线上协议的反序列化类型 ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum RawEvent {
    MessageStart {
        message: RawMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: RawContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: RawDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: RawMessageDelta,
        usage: Option<RawUsage>,
    },
    MessageStop,
    Error {
        error: RawError,
    },
    Ping,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawMessageStart {
    pub(super) id: String,
    pub(super) usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum RawContentBlock {
    Text {
        #[serde(rename = "text")]
        _text: String,
    },
    Thinking {
        #[serde(rename = "thinking")]
        _thinking: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(rename = "input")]
        _input: Option<serde_json::Value>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum RawDelta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    SignatureDelta {
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawMessageDelta {
    pub(super) stop_reason: Option<String>,
}

/// Anthropic 线上 usage 结构（字段名由线上协议决定）。
#[derive(Debug, Deserialize)]
#[allow(clippy::struct_field_names)]
pub(super) struct RawUsage {
    pub(super) input_tokens: Option<u64>,
    pub(super) output_tokens: Option<u64>,
    pub(super) cache_read_input_tokens: Option<u64>,
    pub(super) cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawError {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) message: String,
}
