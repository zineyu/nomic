//! IPC 服务端：Unix socket 上的 JSON Lines 双向协议（#39/#42）。
//!
//! 连接首帧交换协议版本（不兼容即拒绝并关闭）；之后客户端逐帧
//! 发送请求。全部连接的帧经单一队列进入分派循环，串行处理
//! （到达顺序 = 受理顺序）；提交受理层按 requestId 去重（台账在
//! 存储内），受理成功即通知调度器：worker 串行领取待处理会话执行
//! 到静止，失败时向订阅者发出错误事件并把会话挂回待处理。
//!
//! 广播经单一注册表锁出口：会话事件推送给该会话的订阅者，受理的
//! 用户消息同样入事件流（围观方可见对方输入），会话树变更推送给
//! 全部连接——同锁保证各连接收到的相对顺序一致。一方断连只摘除
//! 该连接的订阅，不影响其余连接。启动先做恢复：崩溃残留的执行中
//! 会话回到待处理，与原有待处理工作一起入队重驱。

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nomic_core::{
    ClientMessage, Event, EventErrorKind, ExecutionEnvironment, LlmAdapter, PROTOCOL_VERSION,
    ServerMessage, SessionId, SessionTarget, SessionTreeSnapshot, Storage, StorageError,
};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use crate::agent::{AgentError, RunError, Worker, partial_messages};
use crate::recovery::recoverable_sessions;

use std::os::unix::fs::PermissionsExt;

/// IPC 服务端故障。
#[derive(Debug, Error)]
pub enum IpcError {
    /// socket 目录创建、socket 绑定或权限设置失败。
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// socket 已被活着的 server 占用（并发拉起时只有一个赢家）。
    #[error("socket already in use by a running server: {0}")]
    AddressInUse(String),
    /// 启动恢复读取或写入存储失败。
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// 绑定 socket：父目录按 0700 创建，残留文件清除，socket 权限 0700。
///
/// 返回监听者时 socket 文件已就绪且权限已钉住——客户端等待
/// 「socket 可连接」即可视为 server 拉起成功的信号。
pub async fn bind_listener(socket_path: &Path) -> Result<UnixListener, IpcError> {
    if let Some(parent) = socket_path.parent() {
        // 只创建目录，不改目录权限：目录权限由数据目录所有者
        // （daemon 组合根）决定，bind 不得改写用户既有目录
        tokio::fs::create_dir_all(parent).await?;
    }
    if tokio::fs::try_exists(socket_path).await? {
        // 已被活 server 占用：不得偷取 socket（并发拉起时只有一个赢家）
        if UnixStream::connect(socket_path).await.is_ok() {
            return Err(IpcError::AddressInUse(socket_path.display().to_string()));
        }
        // 连接失败：上次退出未清理的残留，可安全删除
        tokio::fs::remove_file(socket_path).await?;
    }
    let listener = UnixListener::bind(socket_path)?;
    tokio::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o700)).await?;
    Ok(listener)
}

/// 订阅与广播注册表。
///
/// 全部出口共用同一把锁：任何一次广播在锁内发给所有收件人，因此
/// 多次广播的先后对所有连接一致（各连接的发送通道按序投递）。
#[derive(Debug, Default)]
struct Hub {
    /// 连接 id 生成器。
    next_conn: AtomicU64,
    /// 连接集合与订阅关系。
    inner: Mutex<HubInner>,
}

/// 注册表的内部状态。
#[derive(Debug, Default)]
struct HubInner {
    /// 全部活动连接：连接 id → 发送端（树事件广播的收件人集合）。
    connections: HashMap<u64, mpsc::UnboundedSender<ServerMessage>>,
    /// 会话订阅：会话 → 订阅连接的 id 列表。
    sessions: HashMap<SessionId, Vec<u64>>,
}

/// 事件的会话坐标；树事件为全局负载，无会话坐标。
const fn event_session(event: &Event) -> Option<SessionId> {
    match event {
        Event::TokenDelta { session_id, .. }
        | Event::MessageCompleted { session_id, .. }
        | Event::ToolCallStarted { session_id, .. }
        | Event::ToolCallFinished { session_id, .. }
        | Event::Error { session_id, .. } => Some(*session_id),
        Event::SessionTreeChanged { .. } => None,
    }
}

impl Hub {
    fn conn_id(&self) -> u64 {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    /// 登记一个完成握手的连接：成为树事件广播的收件人。
    fn register(&self, conn: u64, tx: mpsc::UnboundedSender<ServerMessage>) {
        self.inner
            .lock()
            .expect("hub poisoned")
            .connections
            .insert(conn, tx);
    }

    /// 连接结束：从全部订阅与广播收件人中移除。
    fn remove_conn(&self, conn: u64) {
        let mut inner = self.inner.lock().expect("hub poisoned");
        inner.connections.remove(&conn);
        inner.sessions.retain(|_, conns| {
            conns.retain(|id| *id != conn);
            !conns.is_empty()
        });
    }

    /// 订阅会话：同连接重复订阅（Subscribe + Resume）只保留一个，
    /// 否则每条事件会被重复投递。
    fn subscribe(&self, conn: u64, session: SessionId) {
        let mut inner = self.inner.lock().expect("hub poisoned");
        let conns = inner.sessions.entry(session).or_default();
        if !conns.contains(&conn) {
            conns.push(conn);
        }
        drop(inner);
    }

    /// 摘除某连接的会话订阅（回放期间避免同一会话的实时事件
    /// 混入历史；回放结束后重新订阅）。
    fn unsubscribe(&self, conn: u64, session: SessionId) {
        let mut inner = self.inner.lock().expect("hub poisoned");
        if let Some(conns) = inner.sessions.get_mut(&session) {
            conns.retain(|id| *id != conn);
            if conns.is_empty() {
                inner.sessions.remove(&session);
            }
        }
    }

    /// 向指定连接发送帧（响应或定向事件）；连接已断开返回 `false`。
    fn send_to(&self, conn: u64, message: ServerMessage) -> bool {
        self.inner
            .lock()
            .expect("hub poisoned")
            .connections
            .get(&conn)
            .is_some_and(|tx| tx.send(message).is_ok())
    }

    /// 会话事件：广播给该会话的全部订阅连接。
    fn broadcast_session_event(&self, event: &Event) {
        let Some(session) = event_session(event) else {
            return;
        };
        let inner = self.inner.lock().expect("hub poisoned");
        let Some(conns) = inner.sessions.get(&session) else {
            return;
        };
        for conn in conns {
            if let Some(tx) = inner.connections.get(conn) {
                let _ = tx.send(ServerMessage::Event(event.clone()));
            }
        }
    }

    /// 会话树变更：广播给全部活动连接。
    fn broadcast_tree(&self, tree: SessionTreeSnapshot) {
        let inner = self.inner.lock().expect("hub poisoned");
        let message = ServerMessage::Event(Event::SessionTreeChanged { tree });
        for tx in inner.connections.values() {
            let _ = tx.send(message.clone());
        }
    }
}

/// 分派请求：一个连接的下一帧。
struct DispatchRequest {
    /// 发起连接的 id。
    conn: u64,
    /// 解出的客户端消息。
    message: ClientMessage,
    /// 处理结果：`true` 继续本连接，`false` 关闭（协议违约或存储不可信）。
    done: oneshot::Sender<bool>,
}

/// 执行失败映射为事件错误类别。
const fn error_kind(error: &AgentError) -> EventErrorKind {
    match error {
        AgentError::Llm(_) => EventErrorKind::Llm,
        AgentError::Environment(_) => EventErrorKind::Environment,
        AgentError::Storage(_)
        | AgentError::PromptIo(_)
        | AgentError::BrokenHistory(_)
        | AgentError::SuspendFailed { .. } => EventErrorKind::Internal,
    }
}

/// IPC 服务端：持有存储与执行依赖，监听 socket 服务协议。
pub struct IpcServer<S, L, E> {
    storage: Arc<S>,
    llm: Arc<L>,
    env: Arc<E>,
    hub: Arc<Hub>,
    /// 调度请求队列：显式请求的会话 id（受理/恢复），串行执行。
    schedule: mpsc::UnboundedSender<SessionId>,
    /// 调度请求接收端；`serve` 启动调度任务时接管。
    pending: Mutex<Option<mpsc::UnboundedReceiver<SessionId>>>,
}

impl<S, L, E> std::fmt::Debug for IpcServer<S, L, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServer").finish_non_exhaustive()
    }
}

impl<S, L, E> IpcServer<S, L, E>
where
    S: Storage + 'static,
    L: LlmAdapter + 'static,
    E: ExecutionEnvironment + 'static,
{
    /// 以共享依赖创建服务端；组件经 `Arc` 持有，连接与调度任务共享。
    pub fn new(storage: Arc<S>, llm: Arc<L>, env: Arc<E>) -> Self {
        let (schedule, pending) = mpsc::unbounded_channel();
        Self {
            storage,
            llm,
            env,
            hub: Arc::new(Hub::default()),
            schedule,
            pending: Mutex::new(Some(pending)),
        }
    }

    /// 绑定 socket 并服务到进程结束。
    pub async fn run(self, socket_path: &Path) -> Result<(), IpcError> {
        let listener = bind_listener(socket_path).await?;
        self.serve(listener).await
    }

    /// 在已绑定的监听者上服务到进程结束。
    pub async fn serve(self, listener: UnixListener) -> Result<(), IpcError> {
        let pending = self
            .pending
            .lock()
            .expect("schedule receiver poisoned")
            .take()
            .expect("serve 只能调用一次");
        // 启动恢复：崩溃残留的执行中会话回到待处理，与原有待处理
        // 工作一起入队；调度器从最后 checkpoint 重驱
        for session in recoverable_sessions(&*self.storage).await? {
            let _ = self.schedule.send(session);
        }
        let (dispatch, requests) = mpsc::unbounded_channel();
        tokio::spawn(scheduler(
            Arc::clone(&self.storage),
            Arc::clone(&self.llm),
            Arc::clone(&self.env),
            Arc::clone(&self.hub),
            pending,
        ));
        tokio::spawn(dispatcher(
            Arc::clone(&self.storage),
            Arc::clone(&self.hub),
            self.schedule.clone(),
            requests,
        ));
        loop {
            let (stream, _) = listener.accept().await?;
            tokio::spawn(handle_connection(
                stream,
                Arc::clone(&self.hub),
                dispatch.clone(),
            ));
        }
    }
}

/// 分派循环：串行处理全部连接的客户端帧。
///
/// 这是 server 的唯一受理串行点——全部连接经同一队列进入，按到达
/// 顺序落库并广播。长耗时的会话执行在 [`scheduler`] 任务中进行，
/// 不阻塞受理。
async fn dispatcher<S: Storage + 'static>(
    storage: Arc<S>,
    hub: Arc<Hub>,
    schedule: mpsc::UnboundedSender<SessionId>,
    mut requests: mpsc::UnboundedReceiver<DispatchRequest>,
) {
    while let Some(request) = requests.recv().await {
        let keep = dispatch(&request.message, request.conn, &*storage, &hub, &schedule)
            .await
            .is_ok();
        let _ = request.done.send(keep);
    }
}

/// 调度器：按到达顺序串行执行显式请求的会话。
///
/// 调度单位是会话 id（受理/恢复/启动重驱），不是全局待处理队列：
/// 失败挂回待处理的会话不会被后续无关请求重复领取，
/// 失败也不会阻塞队列中的其他会话——重驱只能由对该会话的
/// 再次受理（同 requestId 重提）、Resume 或启动恢复显式触发
/// （S10/S11 提供自动重试与无人值守重驱）。
async fn scheduler<S, L, E>(
    storage: Arc<S>,
    llm: Arc<L>,
    env: Arc<E>,
    hub: Arc<Hub>,
    mut pending: mpsc::UnboundedReceiver<SessionId>,
) where
    S: Storage + 'static,
    L: LlmAdapter + 'static,
    E: ExecutionEnvironment + 'static,
{
    let worker = Worker::new(&*storage, &*llm, &*env);
    while let Some(session_id) = pending.recv().await {
        let emit_hub = Arc::clone(&hub);
        let mut emit = move |event: Event| emit_hub.broadcast_session_event(&event);
        // 已执行、请求时已不处于待处理（幂等跳过）或领取时存储
        // 不可信（连接侧另行显式失败）：均无需广播
        match worker.run_session(session_id, &mut emit).await {
            Ok(_) | Err(RunError::Queue(_)) => {}
            // 会话执行失败：已挂回待处理，向订阅者发出错误事件
            Err(RunError::Session(failure)) => {
                hub.broadcast_session_event(&Event::Error {
                    session_id: failure.session,
                    kind: error_kind(&failure.error),
                    message: failure.error.to_string(),
                });
            }
        }
    }
}

/// 读取一帧 JSON Lines 消息；EOF 返回 `Ok(None)`。
async fn read_frame<T: DeserializeOwned>(
    reader: &mut BufReader<OwnedReadHalf>,
) -> std::io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    serde_json::from_str(line.trim_end())
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// 发送一帧 JSON Lines 消息。
async fn write_frame<T: serde::Serialize + Sync>(
    writer: &mut OwnedWriteHalf,
    message: &T,
) -> std::io::Result<()> {
    let mut frame = serde_json::to_vec(message).expect("协议帧序列化失败");
    frame.push(b'\n');
    writer.write_all(&frame).await
}

/// 单连接服务：握手 → 逐帧转发给分派循环，直到 EOF 或协议违约。
async fn handle_connection(
    stream: UnixStream,
    hub: Arc<Hub>,
    dispatch: mpsc::UnboundedSender<DispatchRequest>,
) {
    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let conn = hub.conn_id();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();
    let writer_task = tokio::spawn(async move {
        let mut writer = write_half;
        while let Some(message) = rx.recv().await {
            if write_frame(&mut writer, &message).await.is_err() {
                break;
            }
        }
    });

    // 首帧必须是版本声明；不兼容版本回 HelloAck（携带服务端版本，
    // 客户端据此区分版本不匹配）后关闭
    let Ok(Some(ClientMessage::Hello { protocol_version })) =
        read_frame::<ClientMessage>(&mut reader).await
    else {
        return;
    };
    if tx
        .send(ServerMessage::HelloAck {
            protocol_version: PROTOCOL_VERSION,
        })
        .is_err()
    {
        return;
    }
    if protocol_version != PROTOCOL_VERSION {
        // 关闭前等待写任务把 HelloAck 发出
        drop(tx);
        let _ = writer_task.await;
        return;
    }
    hub.register(conn, tx.clone());

    loop {
        // EOF 或协议违约：关闭连接
        let Ok(Some(message)) = read_frame::<ClientMessage>(&mut reader).await else {
            break;
        };
        let (done, result) = oneshot::channel();
        if dispatch
            .send(DispatchRequest {
                conn,
                message,
                done,
            })
            .is_err()
        {
            break;
        }
        // 分派结果：协议违约或存储不可信时关闭本连接
        if !matches!(result.await, Ok(true)) {
            break;
        }
    }
    hub.remove_conn(conn);
    drop(tx);
    let _ = writer_task.await;
}

/// 分派一帧客户端消息；`Err` 表示协议违约或存储已不可信，关闭连接。
async fn dispatch<S: Storage>(
    message: &ClientMessage,
    conn: u64,
    storage: &S,
    hub: &Hub,
    schedule: &mpsc::UnboundedSender<SessionId>,
) -> Result<(), ()> {
    match message {
        ClientMessage::Hello { .. } => {
            // 握手之后的 Hello 是协议违约
            Err(())
        }
        ClientMessage::Submit { submission } => {
            // 协议无错误响应帧：受理失败（目标不存在/存储故障）
            // 无法给出回执，返回 Err 关闭连接显式失败，避免客户端挂死
            let record = storage
                .record_submission(submission)
                .await
                .map_err(|_| ())?;
            let session = record.acceptance.session_id();
            let created =
                record.message.is_some() && matches!(submission.target, SessionTarget::New);
            let _ = hub.send_to(
                conn,
                ServerMessage::SubmissionReceipt {
                    request_id: submission.request_id,
                    acceptance: record.acceptance,
                },
            );
            // 受理的用户消息进入事件流：订阅方（含旁观者）看到对方输入
            if let Some(user_message) = record.message {
                hub.broadcast_session_event(&Event::MessageCompleted {
                    session_id: session,
                    message: user_message,
                });
            }
            // 受理即调度该会话（重复受理同样触发：请求方以同一
            // requestId 重提即表达重试意图）；先于树广播，使树读取
            // 失败也不会滞留待处理工作
            let _ = schedule.send(session);
            // 新建会话改变树结构：广播给全部连接
            if created {
                hub.broadcast_tree(session_tree(storage).await?);
            }
            Ok(())
        }
        // Cancel 是 fire-and-forget 信号：协议无回执帧，客户端
        // 不可观察「成功」；协作式中断语义在 S10 落地，本切片
        // 只完成协议受理。
        ClientMessage::Cancel { .. } => Ok(()),
        ClientMessage::Subscribe { session_id } => match storage.get_session(*session_id).await {
            Ok(Some(_)) => {
                hub.subscribe(conn, *session_id);
                Ok(())
            }
            // 订阅不存在的会话是客户端契约违约（无错误响应帧）
            Ok(None) | Err(_) => Err(()),
        },
        ClientMessage::ListSessions => {
            let tree = session_tree(storage).await?;
            let _ = hub.send_to(conn, ServerMessage::SessionTree { tree });
            Ok(())
        }
        ClientMessage::Resume { session_id } => {
            // 回放期间摘除该会话订阅：历史必须来自落库步骤，不得混入
            // 同一会话的实时事件；回放结束后重新订阅
            hub.unsubscribe(conn, *session_id);
            let steps = storage.steps(*session_id).await.map_err(|_| ())?;
            // 历史配对违约：会话不可回放，关闭连接显式失败
            let messages = partial_messages(&steps).map_err(|_| ())?;
            for message in messages {
                if !hub.send_to(
                    conn,
                    ServerMessage::Event(Event::MessageCompleted {
                        session_id: *session_id,
                        message,
                    }),
                ) {
                    return Ok(());
                }
            }
            // 同步边界：树快照表示历史回放完毕，此后为实时事件
            let _ = hub.send_to(
                conn,
                ServerMessage::SessionTree {
                    tree: session_tree(storage).await?,
                },
            );
            hub.subscribe(conn, *session_id);
            // 恢复包含重驱：显式请求调度该会话
            let _ = schedule.send(*session_id);
            Ok(())
        }
        ClientMessage::Fork { session_id, at } => {
            storage.fork(*session_id, *at).await.map_err(|_| ())?;
            let tree = session_tree(storage).await?;
            let _ = hub.send_to(conn, ServerMessage::SessionTree { tree: tree.clone() });
            hub.broadcast_tree(tree);
            Ok(())
        }
        ClientMessage::Delete { session_id } => {
            storage.delete(*session_id).await.map_err(|_| ())?;
            let tree = session_tree(storage).await?;
            let _ = hub.send_to(conn, ServerMessage::SessionTree { tree: tree.clone() });
            hub.broadcast_tree(tree);
            Ok(())
        }
    }
}

/// 读取会话树快照；读取失败关闭连接（客户端在等响应帧，静默丢弃
/// 会让它挂死）。
async fn session_tree<S: Storage>(storage: &S) -> Result<SessionTreeSnapshot, ()> {
    storage.session_tree().await.map_err(|_| ())
}
