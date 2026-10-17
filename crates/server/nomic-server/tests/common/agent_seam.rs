//! agent loop seam 支持：脚本化 LLM 替身、脚本化执行环境替身、
//! 事件收集与运行辅助。
//!
//! 剧本耗尽即 panic（fast-fail）：测试多请求一次或多派发一次
//! 都立即失败，不允许静默返回默认值。

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Mutex;

use futures::stream;
use nomic_core::{
    EnvironmentError, Event, ExecutionEnvironment, LlmAdapter, LlmError, LlmEvent, LlmEventStream,
    Message, SessionId, Storage, ToolCall, ToolResult, ToolSpec,
};
use nomic_server::agent::{AgentError, Worker};

/// respond 产出的单个事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptedEvent {
    /// 流式文本增量。
    Token(String),
    /// 请求一次工具调用。
    ToolCall(ToolCall),
}

/// 一次 respond 调用的剧本。
#[derive(Debug)]
pub enum ScriptedTurn {
    /// 产出给定事件序列后正常结束。
    Play(Vec<ScriptedEvent>),
    /// respond 立即以错误失败。
    Fail(LlmError),
}

impl ScriptedTurn {
    /// 纯文本回复剧本。
    pub fn reply(tokens: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Play(
            tokens
                .into_iter()
                .map(|token| ScriptedEvent::Token(token.into()))
                .collect(),
        )
    }
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

/// 脚本化 LLM 替身：响应按剧本推进，请求快照可回放断言。
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
                ScriptedTurn::Play(events) => {
                    let events: Vec<LlmEvent> = events
                        .into_iter()
                        .map(|event| match event {
                            ScriptedEvent::Token(token) => LlmEvent::Token(token),
                            ScriptedEvent::ToolCall(call) => LlmEvent::ToolCallRequested(call),
                        })
                        .collect();
                    let stream: LlmEventStream = Box::pin(stream::iter(events));
                    Ok(stream)
                }
                ScriptedTurn::Fail(error) => Err(error),
            }
        }
    }
}

/// 一次 execute 调用的派发快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedExecution {
    /// 会话 id。
    pub session: SessionId,
    /// 会话工作目录坐标。
    pub working_dir: PathBuf,
    /// 调用内容。
    pub call: ToolCall,
}

/// 脚本化执行环境替身：结果按剧本推进，派发快照可回放断言。
#[derive(Debug)]
pub struct ScriptedEnv {
    outcomes: Mutex<VecDeque<Result<ToolResult, EnvironmentError>>>,
    executions: Mutex<Vec<CapturedExecution>>,
    specs: Vec<ToolSpec>,
}

impl ScriptedEnv {
    /// 按剧本创建替身；`specs` 为对外声明的工具列表。
    pub fn new(outcomes: Vec<Result<ToolResult, EnvironmentError>>, specs: Vec<ToolSpec>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into()),
            executions: Mutex::new(Vec::new()),
            specs,
        }
    }

    /// 无任何剧本的环境：单轮对话测试使用，任何派发都视为剧本越界。
    pub fn inert() -> Self {
        Self::new(Vec::new(), Vec::new())
    }

    /// 已捕获的派发快照（按到达顺序）。
    pub fn executions(&self) -> Vec<CapturedExecution> {
        self.executions
            .lock()
            .expect("captured executions poisoned")
            .clone()
    }
}

impl ExecutionEnvironment for ScriptedEnv {
    fn execute(
        &self,
        session: SessionId,
        working_dir: &std::path::Path,
        call: &ToolCall,
    ) -> impl Future<Output = Result<ToolResult, EnvironmentError>> + Send {
        let outcome = self
            .outcomes
            .lock()
            .expect("scripted outcomes poisoned")
            .pop_front()
            .expect("script exhausted: more tool executions than scripted outcomes");
        self.executions
            .lock()
            .expect("captured executions poisoned")
            .push(CapturedExecution {
                session,
                working_dir: working_dir.to_path_buf(),
                call: call.clone(),
            });
        async move { outcome }
    }

    fn tool_specs(&self) -> &[ToolSpec] {
        &self.specs
    }
}

/// 运行一轮 worker 并收集全部事件：返回运行结果与事件序列。
///
/// 事件与落库步骤的一一对应关系由调用方对照存储断言。
pub async fn run_collect(
    storage: &impl Storage,
    llm: &impl LlmAdapter,
    env: &impl ExecutionEnvironment,
) -> (Result<Option<SessionId>, AgentError>, Vec<Event>) {
    let mut events = Vec::new();
    let result = {
        let mut emit = |event: Event| events.push(event);
        Worker::new(storage, llm, env)
            .run_next_pending(&mut emit)
            .await
    };
    (result, events)
}
