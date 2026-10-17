//! agent loop seam 支持：脚本化 LLM 替身、脚本化执行环境替身、
//! 事件收集与运行辅助。
//!
//! 剧本耗尽即 panic（fast-fail）：测试多请求一次或多派发一次
//! 都立即失败，不允许静默返回默认值。
//!
//! S7 起 seam 经协议驱动：[`ProtocolFixture`] 在临时目录上启动
//! 进程内 server 实例（真实 Unix socket + 注入替身），测试经
//! 协议客户端驱动、直接断言存储与替身快照（值断言与进程内
//! 直调时期一致）。崩溃恢复 seam（S6）经 [`ScriptedOutcome`]
//! 注入工具执行前/后死亡，经 [`ProtocolFixture::restart`] 在同一
//! SQLite 文件上重启实例。

// 共享测试替身模块经 #[path] 引入多个测试二进制（tests/ 每个 .rs
// 独立编译），每个二进制只使用其中一个子集；未使用项在本二进制
// 中不构成死代码。
#![allow(dead_code)]

use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream;
use nomic_core::{
    EnvironmentError, Event, ExecutionEnvironment, LlmAdapter, LlmError, LlmEvent, LlmEventStream,
    Message, SessionId, ToolCall, ToolResult, ToolSpec,
};
use nomic_server::client::Client;
use nomic_server::ipc::IpcServer;
use nomic_server::storage::SQLiteStorage;

/// 测试等待超时：实现缺陷时快速失败而非挂死。
pub const WAIT: Duration = Duration::from_secs(5);

/// 确定性闸门：放行脚本化事件流，钉住「订阅先于事件」等时序。
///
/// 每次放行供给一个事件的发射；提前放行 n 次等价于完全不闸。
#[derive(Debug, Clone)]
pub struct Gate(Arc<tokio::sync::Semaphore>);

impl Gate {
    /// 放行一个事件。
    pub fn release(&self) {
        self.0.add_permits(1);
    }

    /// 放行 n 个事件。
    pub fn release_all(&self, n: usize) {
        self.0.add_permits(n);
    }

    async fn wait(&self) {
        self.0
            .acquire()
            .await
            .expect("gate semaphore closed")
            .forget();
    }
}

/// 创建一个闸门（初始无许可：事件流在放行前阻塞）。
pub fn gate() -> Gate {
    Gate(Arc::new(tokio::sync::Semaphore::new(0)))
}

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
    /// 与 `Play` 相同，但每个事件发射前等待一次闸门放行。
    /// 用于钉住事件流时序（如订阅必须先于事件到达）。
    PlayGated(Gate, Vec<ScriptedEvent>),
    /// 等待闸门放行后立即以错误失败：钉住「订阅先于错误事件」。
    GatedFail(Gate, LlmError),
    /// respond 立即以错误失败。
    Fail(LlmError),
    /// 产出给定事件序列后以错误终止流。
    FailAfter(Vec<ScriptedEvent>, LlmError),
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

    /// 带闸门的文本回复剧本：每个 token 发射前等待一次放行。
    pub fn gated_reply(gate: Gate, tokens: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::PlayGated(
            gate,
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
                    let events = into_llm_events(events);
                    let stream: LlmEventStream = Box::pin(stream::iter(events.into_iter().map(Ok)));
                    Ok(stream)
                }
                ScriptedTurn::PlayGated(gate, events) => {
                    let events = into_llm_events(events);
                    let stream = stream::unfold(
                        (events.into_iter(), gate),
                        |(mut events, gate)| async move {
                            // 每个事件（含流终止）发射前等待一次放行
                            gate.wait().await;
                            let event = events.next()?;
                            Some((Ok(event), (events, gate)))
                        },
                    );
                    let stream: LlmEventStream = Box::pin(stream);
                    Ok(stream)
                }
                ScriptedTurn::FailAfter(events, error) => {
                    let events = into_llm_events(events);
                    let stream: LlmEventStream =
                        Box::pin(stream::iter(events.into_iter().map(Ok).chain([Err(error)])));
                    Ok(stream)
                }
                ScriptedTurn::GatedFail(gate, error) => {
                    gate.wait().await;
                    Err(error)
                }
                ScriptedTurn::Fail(error) => Err(error),
            }
        }
    }
}

/// 剧本事件转为 LLM 事件流负载。
fn into_llm_events(events: Vec<ScriptedEvent>) -> Vec<LlmEvent> {
    events
        .into_iter()
        .map(|event| match event {
            ScriptedEvent::Token(token) => LlmEvent::Token(token),
            ScriptedEvent::ToolCall(call) => LlmEvent::ToolCallRequested(call),
        })
        .collect()
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

/// 一次工具派发的剧本结果。
///
/// 崩溃脚本模拟 server 在工具执行前/后死亡：`CrashBeforeEffect`
/// 在记录派发前永久挂起（副作用未发生）；`CrashAfterEffect` 在记录
/// 派发后永久挂起（副作用已发生、结果未落库）。
#[derive(Debug)]
pub enum ScriptedOutcome {
    /// 派发返回给定结果（成功或环境故障）。
    Return(Result<ToolResult, EnvironmentError>),
    /// 派发前永久挂起：模拟工具执行前崩溃。
    CrashBeforeEffect,
    /// 记录派发后永久挂起：模拟副作用已发生、结果未落库时崩溃。
    CrashAfterEffect,
}

impl From<Result<ToolResult, EnvironmentError>> for ScriptedOutcome {
    fn from(outcome: Result<ToolResult, EnvironmentError>) -> Self {
        Self::Return(outcome)
    }
}

/// 脚本化执行环境替身：结果按剧本推进，派发快照可回放断言。
#[derive(Debug)]
pub struct ScriptedEnv {
    outcomes: Mutex<VecDeque<ScriptedOutcome>>,
    executions: Mutex<Vec<CapturedExecution>>,
    specs: Vec<ToolSpec>,
    /// 派发闸门：每次 execute 在返回结果前等待一次放行。
    gate: Option<Gate>,
}

impl ScriptedEnv {
    /// 按剧本创建替身；`specs` 为对外声明的工具列表。
    pub fn new(
        outcomes: impl IntoIterator<Item = impl Into<ScriptedOutcome>>,
        specs: Vec<ToolSpec>,
    ) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().map(Into::into).collect()),
            executions: Mutex::new(Vec::new()),
            specs,
            gate: None,
        }
    }

    /// 带派发闸门的替身：每次 execute 在返回结果前等待一次放行，
    /// 钉住「调用开始事件到达时执行尚未完成」等时序断言。
    pub fn gated(
        outcomes: impl IntoIterator<Item = impl Into<ScriptedOutcome>>,
        specs: Vec<ToolSpec>,
        gate: Gate,
    ) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().map(Into::into).collect()),
            executions: Mutex::new(Vec::new()),
            specs,
            gate: Some(gate),
        }
    }

    /// 无任何剧本的环境：单轮对话测试使用，任何派发都视为剧本越界。
    pub fn inert() -> Self {
        Self::new(Vec::<ScriptedOutcome>::new(), Vec::new())
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
        working_dir: &Path,
        call: &ToolCall,
    ) -> impl Future<Output = Result<ToolResult, EnvironmentError>> + Send {
        let outcome = self
            .outcomes
            .lock()
            .expect("scripted outcomes poisoned")
            .pop_front()
            .expect("script exhausted: more tool executions than scripted outcomes");
        // 执行前崩溃：副作用未发生，派发不记录
        let record = !matches!(&outcome, ScriptedOutcome::CrashBeforeEffect);
        if record {
            self.executions
                .lock()
                .expect("captured executions poisoned")
                .push(CapturedExecution {
                    session,
                    working_dir: working_dir.to_path_buf(),
                    call: call.clone(),
                });
        }
        let gate = self.gate.clone();
        async move {
            if !record {
                return std::future::pending().await;
            }
            if let Some(gate) = gate {
                gate.wait().await;
            }
            match outcome {
                ScriptedOutcome::Return(result) => result,
                ScriptedOutcome::CrashAfterEffect => std::future::pending().await,
                ScriptedOutcome::CrashBeforeEffect => unreachable!("已按 record 分支"),
            }
        }
    }

    fn tool_specs(&self) -> &[ToolSpec] {
        &self.specs
    }
}

/// 协议 seam 基座：进程内 server 实例（真实 socket + 注入替身）。
///
/// 测试经 [`ProtocolFixture::connect`] 获得协议客户端驱动 server；
/// 存储与替身经 `Arc` 共享，测试直接断言落库与快照（值断言与
/// 进程内直调时期一致）。
#[derive(Debug)]
pub struct ProtocolFixture<L, E> {
    /// 存储：落库断言唯一事实源。
    pub storage: Arc<SQLiteStorage>,
    /// 脚本化 LLM：请求快照回放断言。
    pub llm: Arc<L>,
    /// 执行环境：派发快照回放断言。
    pub env: Arc<E>,
    socket: PathBuf,
    db_path: PathBuf,
    serve: tokio::task::JoinHandle<()>,
}

impl<L, E> ProtocolFixture<L, E>
where
    L: LlmAdapter + 'static,
    E: ExecutionEnvironment + 'static,
{
    /// 在临时目录上启动 server（socket 为 `<dir>/nomic.sock`）。
    pub async fn start(dir: &Path, llm: L, env: E) -> Self {
        let db_path = dir.join("nomic.db");
        let socket = dir.join("nomic.sock");
        let storage = Arc::new(SQLiteStorage::open(&db_path).unwrap());
        let llm = Arc::new(llm);
        let env = Arc::new(env);
        let serve = spawn_server(
            Arc::clone(&storage),
            Arc::clone(&llm),
            Arc::clone(&env),
            &socket,
        );
        wait_for_socket(&socket).await;
        Self {
            storage,
            llm,
            env,
            socket,
            db_path,
            serve,
        }
    }

    /// 模拟进程崩溃后重启：停止服务循环，在同一个 SQLite 文件上
    /// 打开新存储并启动新实例。
    ///
    /// 在途 worker 随旧任务图丢弃（等价于进程死亡）；新实例的启动
    /// 恢复把崩溃残留转回待处理并重驱。返回新实例上完成握手的
    /// 客户端。
    pub async fn restart(&mut self) -> Client {
        self.serve.abort();
        // 等待任务真正结束：旧监听者已释放，随后的 bind 不会撞名
        let _ = (&mut self.serve).await;
        // 清除崩溃现场的 socket 文件；新实例的 bind 也会处理残留，
        // 这里先清使「文件重现」成为可靠的就绪信号
        let _ = tokio::fs::remove_file(&self.socket).await;
        // 残留 socket 文件按崩溃现场处理，由新实例的 bind 逻辑清除
        let storage = Arc::new(SQLiteStorage::open(&self.db_path).unwrap());
        self.serve = spawn_server(
            Arc::clone(&storage),
            Arc::clone(&self.llm),
            Arc::clone(&self.env),
            &self.socket,
        );
        self.storage = storage;
        wait_for_socket(&self.socket).await;
        self.connect().await
    }

    /// 连接并完成版本握手。
    pub async fn connect(&self) -> Client {
        Client::connect(&self.socket).await.unwrap()
    }

    /// socket 路径（权限断言用）。
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }
}

/// 启动进程内 server 实例并返回服务任务句柄。
fn spawn_server<L, E>(
    storage: Arc<SQLiteStorage>,
    llm: Arc<L>,
    env: Arc<E>,
    socket: &Path,
) -> tokio::task::JoinHandle<()>
where
    L: LlmAdapter + 'static,
    E: ExecutionEnvironment + 'static,
{
    let server = IpcServer::new(storage, llm, env);
    let socket = socket.to_path_buf();
    tokio::spawn(async move { server.run(&socket).await.unwrap() })
}

/// 轮询等待 socket 文件出现（绑定完成）。
async fn wait_for_socket(socket: &Path) {
    let start = std::time::Instant::now();
    while !socket.exists() {
        assert!(start.elapsed() < WAIT, "server 未在 {WAIT:?} 内绑定 socket");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 轮询等待条件成立：异步事件驱动下的断言同步点。
pub async fn wait_for(mut cond: impl AsyncFnMut() -> bool) {
    let start = std::time::Instant::now();
    loop {
        if cond().await {
            return;
        }
        assert!(start.elapsed() < WAIT, "等待超时：条件在 {WAIT:?} 内未成立");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 带超时的下一条事件：实现缺陷时快速失败而非挂死。
pub async fn next_event(client: &mut Client) -> Event {
    tokio::time::timeout(WAIT, client.next_event())
        .await
        .expect("等待事件超时")
        .expect("读取事件失败")
        .expect("事件流意外结束")
}

/// 下一条**会话事件**：跳过全局的会话树变更广播。
///
/// 会话树广播发给全部连接，与「事件 ↔ checkpoint 步骤一一对应」
/// 无关；关注会话流的断言用它。
pub async fn next_session_event(client: &mut Client) -> Event {
    loop {
        let event = next_event(client).await;
        if !matches!(event, Event::SessionTreeChanged { .. }) {
            return event;
        }
    }
}

/// 连续读取 n 条事件。
pub async fn read_events(client: &mut Client, n: usize) -> Vec<Event> {
    let mut events = Vec::with_capacity(n);
    for _ in 0..n {
        events.push(next_event(client).await);
    }
    events
}

/// 连续读取 n 条**会话事件**（跳过会话树变更广播）。
pub async fn read_session_events(client: &mut Client, n: usize) -> Vec<Event> {
    let mut events = Vec::with_capacity(n);
    for _ in 0..n {
        events.push(next_session_event(client).await);
    }
    events
}

/// 订阅会话并确认注册生效。
///
/// 写帧完成不代表服务端已处理订阅；同连接帧按序处理，借
/// ListSessions 的树响应作屏障——响应到达时 Subscribe 必已注册，
/// 此后到达的事件不会丢失。
pub async fn subscribe_synced(client: &mut Client, session: SessionId) {
    client.subscribe(session).await.unwrap();
    client.list_sessions().await.unwrap();
}
