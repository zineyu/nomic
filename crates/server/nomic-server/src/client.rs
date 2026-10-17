//! 协议客户端：Unix socket 上的 JSON Lines 双向协议（#39）。
//!
//! 连接即完成版本握手（不匹配显式失败）；请求方法按帧发送并
//! 同步等待回执，事件流经 [`Client::next_event`] 读取。本模块同时
//! 服务 seam 测试与后续 TUI 客户端（S9）。

use std::collections::VecDeque;
use std::path::Path;

use nomic_core::{
    Acceptance, ClientMessage, Event, Message, PROTOCOL_VERSION, ServerMessage, SessionId,
    SessionTreeSnapshot, Submission,
};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// 连接或版本握手失败。
#[derive(Debug, Error)]
pub enum ConnectError {
    /// socket 连接或读写失败。
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// 协议版本与服务端不兼容。
    #[error("protocol version mismatch: server {server}, client {client}")]
    VersionMismatch {
        /// 服务端协议版本。
        server: u32,
        /// 客户端协议版本。
        client: u32,
    },
    /// 握手中连接被关闭，或服务端应答不是 HelloAck（协议违约）。
    #[error("connection closed or violated protocol during handshake")]
    HandshakeFailed,
}

/// 协议客户端：一连接一任务使用（方法调用按序发送并等待回执）。
pub struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    /// 请求等待期间到达的异步事件（按到达顺序；next_event 优先消费）。
    buffered: VecDeque<Event>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// 连接 socket 并完成版本握手。
    ///
    /// # Errors
    ///
    /// socket 连接或帧读写失败，协议版本不匹配，或收到非握手应答时
    /// 返回 [`ConnectError`]。
    pub async fn connect(socket_path: &Path) -> Result<Self, ConnectError> {
        let stream = UnixStream::connect(socket_path).await?;
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        write_frame(
            &mut writer,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await?;
        match read_frame::<ServerMessage>(&mut reader).await? {
            Some(ServerMessage::HelloAck { protocol_version }) => {
                if protocol_version != PROTOCOL_VERSION {
                    return Err(ConnectError::VersionMismatch {
                        server: protocol_version,
                        client: PROTOCOL_VERSION,
                    });
                }
                Ok(Self {
                    reader,
                    writer,
                    buffered: VecDeque::new(),
                })
            }
            _ => Err(ConnectError::HandshakeFailed),
        }
    }

    /// 提交用户消息；返回受理结果（重复 requestId 返回原受理）。
    ///
    /// 新会话的 session id 只能从回执获得，订阅必然晚于受理：
    /// 受理后立即产生的事件可能早于 [`Client::subscribe`]。需要
    /// 完整流（含首批增量）的调用方在回执后调用 [`Client::resume`]
    /// ——回放以落库步骤为准，已 checkpoint 的事件不会缺失。
    ///
    /// # Errors
    ///
    /// 帧读写失败或收到非受理回执时返回 [`std::io::Error`]。
    pub async fn submit(&mut self, submission: &Submission) -> std::io::Result<Acceptance> {
        self.send(&ClientMessage::Submit {
            submission: submission.clone(),
        })
        .await?;
        match self.recv_response().await? {
            ServerMessage::SubmissionReceipt {
                request_id,
                acceptance,
            } if request_id == submission.request_id => Ok(acceptance),
            _ => Err(violation("expected submission receipt")),
        }
    }

    /// 订阅会话事件流。
    ///
    /// # Errors
    ///
    /// 帧读写失败时返回 [`std::io::Error`]。
    pub async fn subscribe(&mut self, session_id: SessionId) -> std::io::Result<()> {
        self.send(&ClientMessage::Subscribe { session_id }).await
    }

    /// 请求会话树快照（列表/树形展示）。
    ///
    /// # Errors
    ///
    /// 帧读写失败或收到非树快照时返回 [`std::io::Error`]。
    pub async fn list_sessions(&mut self) -> std::io::Result<SessionTreeSnapshot> {
        self.send(&ClientMessage::ListSessions).await?;
        expect_tree(self.recv_response().await?)
    }

    /// 恢复会话：回放历史消息（经消息完成事件逐条重放）。
    ///
    /// 返回回放出的完整消息序列；树快照响应帧（[`ServerMessage::SessionTree`]）
    /// 是回放结束边界（收到即历史已同步，之后 [`Client::next_event`]
    /// 读到的是实时事件）。恢复同时订阅会话并重驱其待处理工作。
    ///
    /// # Errors
    ///
    /// 帧读写失败或回放中途收到非预期帧时返回 [`std::io::Error`]。
    pub async fn resume(&mut self, session_id: SessionId) -> std::io::Result<Vec<Message>> {
        self.send(&ClientMessage::Resume { session_id }).await?;
        let mut messages = Vec::new();
        loop {
            match self.recv_frame().await? {
                // 只把目标会话的历史计入回放；其他会话的实时消息入缓冲
                ServerMessage::Event(Event::MessageCompleted {
                    session_id: sid,
                    message,
                }) if sid == session_id => {
                    messages.push(message);
                }
                ServerMessage::SessionTree { .. } => return Ok(messages),
                // 回放期间到达的实时事件（含树变更广播）进入缓冲
                ServerMessage::Event(event) => self.buffered.push_back(event),
                ServerMessage::SubmissionReceipt { .. } | ServerMessage::HelloAck { .. } => {
                    return Err(violation("expected replay messages then tree boundary"));
                }
            }
        }
    }

    /// 在任意消息处分叉会话；返回变化后的会话树快照。
    ///
    /// # Errors
    ///
    /// 帧读写失败或收到非树快照时返回 [`std::io::Error`]。
    pub async fn fork(
        &mut self,
        session_id: SessionId,
        at: nomic_core::MessageId,
    ) -> std::io::Result<SessionTreeSnapshot> {
        self.send(&ClientMessage::Fork { session_id, at }).await?;
        expect_tree(self.recv_response().await?)
    }

    /// 删除会话（级联其子树）；返回变化后的会话树快照。
    ///
    /// # Errors
    ///
    /// 帧读写失败或收到非树快照时返回 [`std::io::Error`]。
    pub async fn delete(&mut self, session_id: SessionId) -> std::io::Result<SessionTreeSnapshot> {
        self.send(&ClientMessage::Delete { session_id }).await?;
        expect_tree(self.recv_response().await?)
    }

    /// 读取下一条事件；连接关闭返回 `Ok(None)`。
    ///
    /// # Errors
    ///
    /// 帧读写失败或收到非事件帧时返回 [`std::io::Error`]。
    pub async fn next_event(&mut self) -> std::io::Result<Option<Event>> {
        if let Some(event) = self.buffered.pop_front() {
            return Ok(Some(event));
        }
        match self.recv_frame().await? {
            ServerMessage::Event(event) => Ok(Some(event)),
            ServerMessage::SubmissionReceipt { .. }
            | ServerMessage::SessionTree { .. }
            | ServerMessage::HelloAck { .. } => Err(violation("expected event")),
        }
    }

    async fn send(&mut self, message: &ClientMessage) -> std::io::Result<()> {
        write_frame(&mut self.writer, message).await
    }

    /// 读取下一帧；连接关闭为显式错误。
    async fn recv_frame(&mut self) -> std::io::Result<ServerMessage> {
        read_frame::<ServerMessage>(&mut self.reader)
            .await?
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
    }

    /// 读取下一帧响应：异步事件按到达顺序缓冲，请求与事件共享
    /// 连接但不互相丢失。
    async fn recv_response(&mut self) -> std::io::Result<ServerMessage> {
        loop {
            match self.recv_frame().await? {
                // 异步事件（含树变更广播）按到达顺序缓冲；非事件帧为响应
                ServerMessage::Event(event) => self.buffered.push_back(event),
                message => return Ok(message),
            }
        }
    }
}

/// 取响应里的树快照；其他帧为协议违约。
fn expect_tree(message: ServerMessage) -> std::io::Result<SessionTreeSnapshot> {
    match message {
        ServerMessage::SessionTree { tree } => Ok(tree),
        _ => Err(violation("expected session tree")),
    }
}

/// 协议违约：服务端应答帧与请求不成对。
fn violation(context: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("protocol violation: {context}"),
    )
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
