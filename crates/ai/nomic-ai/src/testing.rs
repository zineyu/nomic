//! 剧本化 rig 适配器：以 OpenAI Chat SSE 帧驱动真实 wire 编解码。
//!
//! 供 server 的 seam 测试把既有剧本换成 rig 适配器后重跑：每个回合
//! 编码为一组 `chat.completion.chunk` 帧，经 rig 官方
//! `SequencedHttpClient` 送达，请求原文可读回用于断言系统提示、
//! 工具声明与历史序列。不触网。

use std::sync::{Arc, Mutex};

use nomic_core::{LlmAdapter, LlmError, LlmEventStream, Message, SessionId, ToolSpec};
use rig_core::providers::openai::OpenAIConfig;
use rig_core::test_utils::SequencedHttpClient;

use crate::RigAdapter;
use crate::sse;

pub use crate::sse::{SCRIPTED_MODEL, ScriptedEvent, ScriptedTurn};

/// 剧本端点的基址。
pub const SCRIPTED_BASE_URL: &str = "http://scripted.invalid/v1";
/// 剧本密钥：只出现在 Authorization 头，不进入任何落盘内容。
pub const SCRIPTED_API_KEY: &str = "scripted-api-key";

/// respond 收到的领域请求快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRequest {
    /// 请求的会话 id。
    pub session: SessionId,
    /// 完整消息历史（以系统提示开头）。
    pub messages: Vec<Message>,
    /// 传入的工具声明。
    pub tools: Vec<ToolSpec>,
}

/// wire 上实际发出的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentRequest {
    /// 请求 URL。
    pub url: String,
    /// Authorization 头（若存在）。
    pub authorization: Option<String>,
    /// 请求体原文。
    pub body: String,
}

/// 剧本化 rig 适配器：按剧本返回 SSE 回复，并记录请求。
#[derive(Clone, Debug)]
pub struct ScriptedRig {
    adapter: RigAdapter,
    http: SequencedHttpClient,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl ScriptedRig {
    /// 按剧本创建适配器。
    pub fn new(turns: impl IntoIterator<Item = ScriptedTurn>) -> Self {
        let responses = turns
            .into_iter()
            .map(|turn| sse::response(&turn))
            .collect::<Vec<_>>();
        let http = SequencedHttpClient::new(responses);
        let config = OpenAIConfig::new(SCRIPTED_API_KEY).with_base_url(SCRIPTED_BASE_URL);
        Self {
            adapter: RigAdapter::from_model(config.connect(http.clone()).chat(SCRIPTED_MODEL)),
            http,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 已捕获的领域请求（按到达顺序）。
    pub fn captured(&self) -> Vec<CapturedRequest> {
        self.requests
            .lock()
            .expect("captured requests poisoned")
            .clone()
    }

    /// wire 上实际发出的请求（按到达顺序）。
    pub fn sent(&self) -> Vec<SentRequest> {
        self.http
            .requests()
            .into_iter()
            .map(|request| SentRequest {
                url: request.uri,
                authorization: request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
                body: String::from_utf8_lossy(&request.body).into_owned(),
            })
            .collect()
    }
}

impl LlmAdapter for ScriptedRig {
    async fn respond(
        &self,
        session: SessionId,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> Result<LlmEventStream, LlmError> {
        self.requests
            .lock()
            .expect("captured requests poisoned")
            .push(CapturedRequest {
                session,
                messages: messages.to_vec(),
                tools: tools.to_vec(),
            });
        self.adapter.respond(session, messages, tools).await
    }
}
