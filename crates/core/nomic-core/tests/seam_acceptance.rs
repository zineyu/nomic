//! S1 规格级验收测试：只经 nomic-core 公共 API，对应四个测试 seam。
//!
//! ① 会话树分叉（父子独立）② requestId 去重语义
//! ③ 事件与客户端→server 请求的 JSON Lines 编解码 ④ 会话执行状态转换。

use nomic_core::{
    Acceptance, ClientMessage, Event, EventErrorKind, Message, MessageId, PROTOCOL_VERSION,
    ParentRef, RequestId, Role, ServerMessage, SessionId, SessionState, SessionTarget, SessionTree,
    StateTransitionError, Step, Submission, ToolCall, ToolCallId, ToolName, ToolResult, TreeError,
};

// ---------- seam ①：会话树分叉（父子独立） ----------

#[test]
fn 分叉点消息_id_记录于父子引用() {
    let mut tree = SessionTree::new();
    let root = tree.create_root("/tmp/ws");
    let at = MessageId::new();
    tree.append_message(
        root,
        Message::User {
            id: at,
            text: "m1".into(),
        },
    )
    .unwrap();
    let child = tree.fork(root, at).unwrap();

    let parent_ref = tree.get(child).unwrap().parent().unwrap();
    assert_eq!(
        parent_ref,
        ParentRef {
            session: root,
            forked_at: at
        }
    );
}

#[test]
fn 同一消息可分叉出相互独立的兄弟会话() {
    let mut tree = SessionTree::new();
    let root = tree.create_root("/tmp/ws");
    let at = MessageId::new();
    tree.append_message(
        root,
        Message::User {
            id: at,
            text: "m1".into(),
        },
    )
    .unwrap();
    let left = tree.fork(root, at).unwrap();
    let right = tree.fork(root, at).unwrap();

    assert_ne!(left, right);
    assert_eq!(tree.len(), 3);
    let children: Vec<SessionId> = tree.children_of(root).iter().map(|s| s.id()).collect();
    assert_eq!(children.len(), 2);
    assert!(children.contains(&left));
    assert!(children.contains(&right));
    // 兄弟分支互不隶属：left 的 children 不含 right
    assert!(!tree.children_of(left).iter().any(|s| s.id() == right));
}

#[test]
fn 分叉会话独立于父会话且继承工作目录() {
    let mut tree = SessionTree::new();
    let root = tree.create_root("/tmp/ws");
    let at = MessageId::new();
    tree.append_message(
        root,
        Message::User {
            id: at,
            text: "m1".into(),
        },
    )
    .unwrap();
    let before = tree.get(root).unwrap().clone();
    let child = tree.fork(root, at).unwrap();

    let child_session = tree.get(child).unwrap();
    assert_ne!(child_session.id(), root);
    assert_eq!(child_session.working_dir(), before.working_dir());
    // 分叉不改变来源会话
    assert_eq!(tree.get(root).unwrap(), &before);
}

#[test]
fn 从不存在会话分叉返回规格错误() {
    let mut tree = SessionTree::new();
    let result = tree.fork(SessionId::new(), MessageId::new());
    assert!(matches!(result, Err(TreeError::SessionMissing(_))));
}

#[test]
fn 树形列表正确反映从属关系() {
    let mut tree = SessionTree::new();
    let other_root = tree.create_root("/tmp/other");
    let root = tree.create_root("/tmp/ws");
    let m1 = MessageId::new();
    tree.append_message(
        root,
        Message::User {
            id: m1,
            text: "m1".into(),
        },
    )
    .unwrap();
    let child = tree.fork(root, m1).unwrap();
    let grandchild = tree.fork(child, m1).unwrap();

    let roots: Vec<SessionId> = tree.roots().iter().map(|s| s.id()).collect();
    assert_eq!(roots.len(), 2);
    assert!(roots.contains(&root));
    assert!(roots.contains(&other_root));
    assert_eq!(
        tree.children_of(root)
            .iter()
            .map(|s| s.id())
            .collect::<Vec<_>>(),
        vec![child]
    );
    assert_eq!(
        tree.children_of(child)
            .iter()
            .map(|s| s.id())
            .collect::<Vec<_>>(),
        vec![grandchild]
    );
    assert!(tree.children_of(grandchild).is_empty());
    // 子会话不出现在根列表
    assert!(!roots.contains(&child));
}

#[test]
fn 分叉后父子消息追加互不可见() {
    // 写时复制：分叉复制至分叉点（含）的消息前缀，此后各自追加互不可见
    let mut tree = SessionTree::new();
    let root = tree.create_root("/tmp/ws");
    let m1 = MessageId::new();
    tree.append_message(
        root,
        Message::User {
            id: m1,
            text: "m1".into(),
        },
    )
    .unwrap();
    let child = tree.fork(root, m1).unwrap();

    let m2 = MessageId::new();
    tree.append_message(
        root,
        Message::User {
            id: m2,
            text: "root only".into(),
        },
    )
    .unwrap();
    let c1 = MessageId::new();
    tree.append_message(
        child,
        Message::User {
            id: c1,
            text: "child only".into(),
        },
    )
    .unwrap();

    let root_ids: Vec<MessageId> = tree
        .messages(root)
        .unwrap()
        .iter()
        .map(Message::id)
        .collect();
    let child_ids: Vec<MessageId> = tree
        .messages(child)
        .unwrap()
        .iter()
        .map(Message::id)
        .collect();
    assert_eq!(root_ids, vec![m1, m2]);
    assert_eq!(child_ids, vec![m1, c1]);
}

// ---------- seam ②：受理请求 requestId 去重语义 ----------

#[test]
fn 重发提交经协议后仍持有同一去重键() {
    let request_id = RequestId::new();
    let original = Submission {
        request_id,
        target: SessionTarget::New,
        content: "原始任务".into(),
    };
    // 网络重发：request_id 相同，其余字段已被客户端改动
    let resent = Submission {
        request_id,
        target: SessionTarget::Existing(SessionId::new()),
        content: "重发时已不同的文本".into(),
    };

    let wire_original = serde_json::to_string(&original).unwrap();
    let wire_resent = serde_json::to_string(&resent).unwrap();
    let decoded_original: Submission = serde_json::from_str(&wire_original).unwrap();
    let decoded_resent: Submission = serde_json::from_str(&wire_resent).unwrap();

    // 去重只看 request_id：重发后键不变，内容/目标差异不影响键
    assert_eq!(decoded_original.request_id, decoded_resent.request_id);
    assert_ne!(decoded_original.content, decoded_resent.content);
    assert_ne!(decoded_original, decoded_resent);
}

#[test]
fn 重复受理回执携带原会话_id_且不是错误() {
    let session_id = SessionId::new();
    let receipt = ServerMessage::SubmissionReceipt {
        request_id: RequestId::new(),
        acceptance: Acceptance::Duplicate { session_id },
    };
    let line = serde_json::to_string(&receipt).unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();

    // Duplicate 以普通受理变体编码，不携带 error 语义
    assert!(value.get("submission_receipt").is_some());
    let acceptance = &value["submission_receipt"]["acceptance"];
    assert!(acceptance.get("duplicate").is_some());
    assert!(acceptance.get("error").is_none());
    assert_eq!(
        acceptance["duplicate"]["session_id"].as_str().unwrap(),
        session_id.to_string()
    );

    // 客户端据回执得知「已受理过」：与首次受理给出同一会话 id 时无需重试
    let decoded = serde_json::from_str::<ServerMessage>(&line).unwrap();
    assert_eq!(decoded, receipt);
}

#[test]
fn 受理回执经_request_id_关联到提交() {
    let request_id = RequestId::new();
    let submission = Submission {
        request_id,
        target: SessionTarget::New,
        content: "任务".into(),
    };
    let wire = serde_json::to_string(&ClientMessage::Submit { submission }).unwrap();
    let decoded = serde_json::from_str::<ClientMessage>(&wire).unwrap();
    let ClientMessage::Submit { submission } = decoded else {
        panic!("submit 解码失败");
    };
    assert_eq!(submission.request_id, request_id);
}

// ---------- seam ③：事件与客户端→server 请求的 JSON Lines 编解码 ----------

fn sample_call() -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        tool: ToolName::new("read_file"),
        arguments: serde_json::json!({"path": "src/lib.rs"}),
    }
}

fn all_client_messages() -> Vec<ClientMessage> {
    vec![
        ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
        ClientMessage::Submit {
            submission: Submission {
                request_id: RequestId::new(),
                target: SessionTarget::Existing(SessionId::new()),
                content: "继续任务".into(),
            },
        },
        ClientMessage::Cancel {
            session_id: SessionId::new(),
        },
        ClientMessage::Subscribe {
            session_id: SessionId::new(),
        },
        ClientMessage::ListSessions,
        ClientMessage::Resume {
            session_id: SessionId::new(),
        },
        ClientMessage::Fork {
            session_id: SessionId::new(),
            at: MessageId::new(),
        },
        ClientMessage::Delete {
            session_id: SessionId::new(),
        },
    ]
}

fn all_events() -> Vec<Event> {
    let session_id = SessionId::new();
    let message_id = MessageId::new();
    let call = sample_call();
    vec![
        Event::TokenDelta {
            session_id,
            message_id,
            delta: "增量".into(),
        },
        Event::MessageCompleted {
            session_id,
            message: Message::Assistant {
                id: message_id,
                text: "完成".into(),
                tool_calls: vec![call.clone()],
            },
        },
        Event::ToolCallStarted {
            session_id,
            call: call.clone(),
        },
        Event::ToolCallFinished {
            session_id,
            call_id: call.id,
            result: ToolResult::ok("输出"),
        },
        Event::ToolCallFinished {
            session_id,
            call_id: call.id,
            result: ToolResult::rejected("越出工作目录"),
        },
        Event::Error {
            session_id,
            kind: EventErrorKind::ContextLimit,
            message: "上下文上限".into(),
        },
        Event::SessionTreeChanged {
            tree: SessionTree::new().snapshot(),
        },
    ]
}

/// 协议载体是 JSON Lines：编码为一行、按行切分、逐帧还原。
fn assert_json_lines_round_trip<T>(frames: &[T])
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let mut buffer = String::new();
    for frame in frames {
        let line = serde_json::to_string(frame).unwrap();
        assert!(!line.contains('\n'), "帧必须恰好占一行");
        buffer.push_str(&line);
        buffer.push('\n');
    }
    let restored: Vec<T> = buffer
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(restored, frames);
}

#[test]
fn 客户端请求全集按_json_lines_往返() {
    assert_json_lines_round_trip(&all_client_messages());
}

#[test]
fn 事件全集经服务端消息按_json_lines_往返() {
    let frames: Vec<ServerMessage> = all_events().into_iter().map(ServerMessage::Event).collect();
    assert_json_lines_round_trip(&frames);
}

#[test]
fn 协议帧标签为_snake_case_可读形式() {
    let tags = [
        serde_json::to_value(ClientMessage::ListSessions).unwrap(),
        serde_json::to_value(ClientMessage::Submit {
            submission: Submission {
                request_id: RequestId::new(),
                target: SessionTarget::New,
                content: "x".into(),
            },
        })
        .unwrap(),
        serde_json::to_value(Event::TokenDelta {
            session_id: SessionId::new(),
            message_id: MessageId::new(),
            delta: "d".into(),
        })
        .unwrap(),
        serde_json::to_value(Event::SessionTreeChanged {
            tree: SessionTree::new().snapshot(),
        })
        .unwrap(),
    ];
    // 单元变体序列化为裸字符串标签，带负载变体序列化为 {标签: 负载}
    assert_eq!(tags[0], serde_json::json!("list_sessions"));
    assert!(tags[1].get("submit").is_some());
    assert!(tags[2].get("token_delta").is_some());
    assert!(tags[3].get("session_tree_changed").is_some());
}

#[test]
fn 连接首帧交换协议版本() {
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
    };
    let ack = ServerMessage::HelloAck {
        protocol_version: PROTOCOL_VERSION,
    };
    assert_json_lines_round_trip(&[hello]);
    assert_json_lines_round_trip(std::slice::from_ref(&ack));
    // 版本字段在帧内可见，供对端做兼容判断
    let value = serde_json::to_value(&ack).unwrap();
    assert_eq!(
        value["hello_ack"]["protocol_version"].as_u64().unwrap(),
        u64::from(PROTOCOL_VERSION)
    );
}

#[test]
fn 事件负载携带完整领域语义() {
    // 工具调用开始事件携带调用内容，完成事件携带结果并引用同一调用 id
    let session_id = SessionId::new();
    let call = sample_call();
    let started = Event::ToolCallStarted {
        session_id,
        call: call.clone(),
    };
    let finished = Event::ToolCallFinished {
        session_id,
        call_id: call.id,
        result: ToolResult::ok("done"),
    };
    let ClientMessage::Submit { submission } = all_client_messages()[1].clone() else {
        panic!("submit 分支构造错误")
    };
    assert_eq!(submission.content, "继续任务");
    assert!(matches!(
        started,
        Event::ToolCallStarted {
            call: ToolCall { id, .. },
            ..
        } if id == finished_call_id(&finished)
    ));
}

fn finished_call_id(event: &Event) -> ToolCallId {
    let Event::ToolCallFinished { call_id, .. } = event else {
        panic!("非工具完成事件")
    };
    *call_id
}

// ---------- seam ④：会话执行状态转换 ----------

#[test]
fn 完整生命周期_待处理到运行中到静止() {
    assert_eq!(
        SessionState::Pending.dispatch().unwrap(),
        SessionState::Running
    );
    assert_eq!(SessionState::Running.quiesce().unwrap(), SessionState::Idle);
}

#[test]
fn worker_死亡后回到待处理并可重驱() {
    // 运行中 → 待处理（崩溃/死亡），随后可被调度器再次取走
    let recovered = SessionState::Running.suspend().unwrap();
    assert_eq!(recovered, SessionState::Pending);
    assert_eq!(recovered.dispatch().unwrap(), SessionState::Running);
}

#[test]
fn 非法转换返回错误且错误可区分() {
    assert!(matches!(
        SessionState::Running.dispatch(),
        Err(StateTransitionError { .. })
    ));
    assert!(SessionState::Idle.suspend().is_err());
    assert!(SessionState::Pending.quiesce().is_err());
    assert!(SessionState::Idle.quiesce().is_err());

    // 错误信息指明来源状态与操作
    let err = SessionState::Running.dispatch().unwrap_err();
    let text = err.to_string();
    assert!(text.contains("Running"));
    assert!(text.contains("dispatch"));
}

#[test]
fn 状态序列化为_snake_case_标签() {
    for (state, tag) in [
        (SessionState::Pending, "pending"),
        (SessionState::Running, "running"),
        (SessionState::Idle, "idle"),
    ] {
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, format!("\"{tag}\""));
        assert_eq!(serde_json::from_str::<SessionState>(&json).unwrap(), state);
    }
}

// ---------- 公共导出面的完整性 ----------

#[test]
fn 领域词汇全部可从_crate_根导入() {
    // Step 与 Role 在协议与存储契约中出现，须为公共 API
    let _ = Step::Message(Message::User {
        id: MessageId::new(),
        text: "x".into(),
    });
    let _ = Role::Assistant;
    let _ = Message::Tool {
        id: MessageId::new(),
        call_id: ToolCallId::new(),
        result: ToolResult::rejected("r"),
    };
}
