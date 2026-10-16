//! agent loop seam 支持：脚本化 LLM 替身、事件收集与运行辅助。
//!
//! 剧本耗尽即 panic（fast-fail）：测试多请求一次或少排一幕
//! 都立即失败，不允许静默返回默认值。

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;

use futures::stream;
use nomic_core::{
    Event, LlmAdapter, LlmError, LlmEvent, LlmEventStream, Message, SessionId, Storage, ToolCall,
    ToolCallId, ToolName, ToolSpec,
};
use nomic_server::agent::{AgentError, Worker};

/// 一次 respond 调用的剧本。
#[derive(Debug)]
pub enum ScriptedTurn {
    /// 产出流式 token 序列后正常结束。
    Reply(Vec<String>),
    /// 流中途请求一次工具调用（未注册工具时的协议违约剧本）。
    RequestToolCall,
    /// respond 立即以错误失败。
    Fail(LlmError),
}

/// respond 收到的请求快照（断言系统提示构成与历史序列）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRequest {
    /// 请求的会话 id。
    pub session: SessionId,
    /// 完整消息历史（以系统提示开头）。
    pub messages: Vec<Message>,
    /// 传入的工具声明。
    pub tools: Vec<ToolSpec>,
}

/// 脚本化替身：响应按剧本推进，请求快照可回放断言。
#[derive(Debug)]
pub struct ScriptedLlm {
    turns: Mutex<VecDeque<ScriptedTurn>>,
    requests: Mutex<Vec<CapturedRequest>>,
}

impl ScriptedLlm {
    /// 按剧本创建替身。
    pub fn new(turns: Vec<ScriptedTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// 已捕获的请求快照（按到达顺序）。
    pub fn captured(&self) -> Vec<CapturedRequest> {
        self.requests
            .lock()
            .expect("captured requests poisoned")
            .clone()
    }
}

impl LlmAdapter for ScriptedLlm {
    fn respond(
        &self,
        session: SessionId,
        messages: &[Message],
        tools: &[ToolSpec],
    ) -> impl Future<Output = Result<LlmEventStream, LlmError>> + Send {
        let turn = self
            .turns
            .lock()
            .expect("scripted turns poisoned")
            .pop_front()
            .expect("script exhausted: more LLM requests than scripted turns");
        self.requests
            .lock()
            .expect("captured requests poisoned")
            .push(CapturedRequest {
                session,
                messages: messages.to_vec(),
                tools: tools.to_vec(),
            });
        async move {
            match turn {
                ScriptedTurn::Reply(tokens) => {
                    let stream: LlmEventStream =
                        Box::pin(stream::iter(tokens.into_iter().map(LlmEvent::Token)));
                    Ok(stream)
                }
                ScriptedTurn::RequestToolCall => {
                    let call = ToolCall {
                        id: ToolCallId::new(),
                        tool: ToolName::new("shell"),
                        arguments: serde_json::json!({"command": "cargo test"}),
                    };
                    let stream: LlmEventStream =
                        Box::pin(stream::iter([LlmEvent::ToolCallRequested(call)]));
                    Ok(stream)
                }
                ScriptedTurn::Fail(error) => Err(error),
            }
        }
    }
}

/// 运行一轮 worker 并收集全部事件：返回运行结果与事件序列。
///
/// 事件与落库步骤的一一对应关系由调用方对照存储断言。
pub async fn run_collect(
    storage: &impl Storage,
    llm: &impl LlmAdapter,
) -> (Result<Option<SessionId>, AgentError>, Vec<Event>) {
    let mut events = Vec::new();
    let result = {
        let mut emit = |event: Event| events.push(event);
        Worker::new(storage, llm).run_next_pending(&mut emit).await
    };
    (result, events)
}
