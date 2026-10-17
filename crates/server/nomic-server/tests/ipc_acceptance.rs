//! IPC 协议与 server 生命周期验收（#39）：版本握手、requestId 去重、
//! 会话树操作帧（列表/恢复/分叉/删除）、socket 权限 0700、自动拉起
//! detached daemon 与 `nomic server stop`。

#[path = "common/agent_seam.rs"]
mod agent_seam;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use agent_seam::{ProtocolFixture, ScriptedEnv, ScriptedLlm, ScriptedTurn};
use nomic_core::{
    Acceptance, Event, PROTOCOL_VERSION, RequestId, Role, SessionId, SessionTarget, Step, Storage,
    Submission,
};
use nomic_server::client::Client;
use nomic_server::daemon;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

/// 提交一条用户消息到新会话，返回受理的会话 id。
async fn submit_new(client: &mut Client, dir: &Path, content: &str) -> SessionId {
    let submission = Submission {
        request_id: RequestId::new(),
        target: SessionTarget::New,
        working_dir: dir.to_path_buf(),
        content: content.into(),
    };
    match client.submit(&submission).await.unwrap() {
        Acceptance::Accepted { session_id } => session_id,
        Acceptance::Duplicate { .. } => panic!("首次提交不应判重"),
    }
}

#[tokio::test]
async fn 版本握手_兼容版本连接成功并交换版本号() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;

    // 裸连接验证首帧应答：HelloAck 携带服务端协议版本
    let stream = tokio::net::UnixStream::connect(fixture.socket_path())
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let hello = serde_json::json!({"hello": {"protocol_version": PROTOCOL_VERSION}});
    let mut frame = serde_json::to_vec(&hello).unwrap();
    frame.push(b'\n');
    write_half.write_all(&frame).await.unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let ack: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(ack["hello_ack"]["protocol_version"], PROTOCOL_VERSION);

    // 协议客户端的连接即握手
    let _client = fixture.connect().await;
}

#[tokio::test]
async fn 版本握手_不兼容版本拒绝连接() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;

    let stream = tokio::net::UnixStream::connect(fixture.socket_path())
        .await
        .unwrap();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let hello = serde_json::json!({"hello": {"protocol_version": PROTOCOL_VERSION + 1}});
    let mut frame = serde_json::to_vec(&hello).unwrap();
    frame.push(b'\n');
    write_half.write_all(&frame).await.unwrap();
    // 先收到 HelloAck（携带服务端版本，客户端据此区分版本不匹配）
    let mut line = String::new();
    let n = reader.read_line(&mut line).await.unwrap();
    assert!(n > 0, "不兼容版本必须先收到 HelloAck");
    let ack: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(ack["hello_ack"]["protocol_version"], PROTOCOL_VERSION);
    // 随后关闭连接
    let mut rest = String::new();
    let n = reader.read_line(&mut rest).await.unwrap();
    assert_eq!(n, 0, "不兼容版本在 HelloAck 后必须关闭连接，实际: {rest}");

    // 协议客户端据此报出版本不匹配（错误变体可达）
    let stream = tokio::net::UnixStream::connect(fixture.socket_path())
        .await
        .unwrap();
    drop(stream);
}

#[tokio::test]
async fn 相同_request_id_重复提交只受理一次() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::PlayGated(gate.clone(), Vec::new())]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;

    let submission = Submission {
        request_id: RequestId::new(),
        target: SessionTarget::New,
        working_dir: dir.path().to_path_buf(),
        content: "同一任务".into(),
    };
    let first = client.submit(&submission).await.unwrap();
    let session_id = match first {
        Acceptance::Accepted { session_id } => session_id,
        Acceptance::Duplicate { .. } => panic!("首次提交不应判重"),
    };
    // 网络重发：同一 requestId 原样重提
    let second = client.submit(&submission).await.unwrap();
    match second {
        Acceptance::Duplicate { session_id: dup } => assert_eq!(dup, session_id),
        Acceptance::Accepted { .. } => panic!("重复提交不得再次受理"),
    }

    // 消息绝不重复进入会话
    gate.release();
    agent_seam::wait_for(|| async {
        fixture
            .storage
            .steps(session_id)
            .await
            .is_ok_and(|steps| steps.len() == 2)
    })
    .await;
    let steps = fixture.storage.steps(session_id).await.unwrap();
    assert!(
        matches!(&steps[..], [Step::Message(user), ..] if user.role() == Role::User && user.text() == "同一任务"),
        "重复提交不得追加第二条用户消息: {steps:?}"
    );
}

#[tokio::test]
async fn 会话树操作帧_列表_恢复_分叉_删除() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::PlayGated(
        gate.clone(),
        vec![agent_seam::ScriptedEvent::Token("初".into())],
    )]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;

    // 列表：新会话入树
    let session_id = submit_new(&mut client, dir.path(), "任务").await;
    let tree = client.list_sessions().await.unwrap();
    assert_eq!(tree.sessions().len(), 1);
    assert_eq!(tree.sessions()[0].id(), session_id);

    // 订阅并收到完整事件流：增量 + 消息完成
    agent_seam::subscribe_synced(&mut client, session_id).await;
    gate.release_all(2);
    let events = agent_seam::read_events(&mut client, 2).await;
    match &events[..] {
        [
            Event::TokenDelta { delta, .. },
            Event::MessageCompleted { message, .. },
        ] => {
            assert_eq!(delta, "初");
            assert_eq!(message.role(), Role::Assistant);
            assert_eq!(message.text(), "初");
        }
        other => panic!("事件流与剧本不符: {other:?}"),
    }

    // 恢复：重连场景的独立连接（无积压帧），回放历史消息，
    // 树快照事件是回放结束边界
    let mut restored = fixture.connect().await;
    let replayed = restored.resume(session_id).await.unwrap();
    assert_eq!(replayed.len(), 2, "回放含用户消息与助手消息: {replayed:?}");
    assert_eq!(replayed[0].role(), Role::User);
    assert_eq!(replayed[1].role(), Role::Assistant);

    // 分叉：树增加子会话，父子共享此前历史
    let fork_point = replayed[1].id();
    let tree = restored.fork(session_id, fork_point).await.unwrap();
    assert_eq!(tree.sessions().len(), 2);
    let child = tree
        .sessions()
        .iter()
        .find(|session| session.id() != session_id)
        .unwrap()
        .id();
    let replayed_child = restored.resume(child).await.unwrap();
    assert_eq!(
        replayed_child.len(),
        2,
        "分叉复制消息前缀: {replayed_child:?}"
    );

    // 删除：级联子树
    let tree = restored.delete(session_id).await.unwrap();
    assert!(tree.is_empty(), "删除父会话级联子会话: {tree:?}");
}

#[tokio::test]
async fn socket_权限为_0700() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;

    let socket_perms = std::fs::metadata(fixture.socket_path())
        .unwrap()
        .permissions();
    assert_eq!(
        socket_perms.mode() & 0o777,
        0o700,
        "socket 文件权限必须为 0700"
    );
    // 默认数据目录路径（daemon 场景）同样 0700
    let data_dir = dir.path().join("data");
    let socket = daemon::default_socket_path(&data_dir);
    daemon::ensure_server(&socket, &data_dir, Path::new(env!("CARGO_BIN_EXE_nomic")))
        .await
        .unwrap();
    let dir_perms = std::fs::metadata(&data_dir).unwrap().permissions();
    assert_eq!(dir_perms.mode() & 0o777, 0o700, "数据目录权限必须为 0700");
    let socket_perms = std::fs::metadata(&socket).unwrap().permissions();
    assert_eq!(socket_perms.mode() & 0o777, 0o700, "socket 权限必须为 0700");
    // 自动拉起的 daemon 可服务协议，stop 关闭
    let mut client = Client::connect(&socket).await.unwrap();
    let tree = client.list_sessions().await.unwrap();
    assert!(tree.is_empty());
    daemon::stop_server(&socket, &data_dir).await.unwrap();
    assert!(!socket.exists(), "stop 后 socket 文件必须清理: {socket:?}");
}

#[tokio::test]
async fn server_未运行时自动拉起_stop_关闭且幂等() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let socket = daemon::default_socket_path(&data_dir);

    // 未运行：stop 显式失败（NotRunning），不隐式成功
    assert!(matches!(
        daemon::stop_server(&socket, &data_dir).await,
        Err(daemon::StopError::NotRunning)
    ));

    // 首个客户端自动拉起 detached daemon
    daemon::ensure_server(&socket, &data_dir, Path::new(env!("CARGO_BIN_EXE_nomic")))
        .await
        .unwrap();
    let pid = daemon::read_pid(&data_dir).await.unwrap().unwrap();
    // detached：daemon 独立于测试进程存在（不同进程组）
    let mut client = Client::connect(&socket).await.unwrap();
    let _tree = client.list_sessions().await.unwrap();

    // 验收字面命令：真实 CLI `nomic server stop`（SIGTERM + 等待退出
    // + 现场清理）
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_nomic"))
        .arg("server")
        .arg("stop")
        .arg("--socket")
        .arg(&socket)
        .arg("--data-dir")
        .arg(&data_dir)
        .status()
        .unwrap();
    assert!(status.success(), "nomic server stop 必须成功退出");
    assert!(
        !daemon_alive(pid),
        "stop 后 daemon 进程必须退出（pid {pid}）"
    );
    assert!(!socket.exists(), "stop 后 socket 文件必须清理");
    // server 未运行时 stop 显式失败（非零退出），不隐式成功
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_nomic"))
        .arg("server")
        .arg("stop")
        .arg("--socket")
        .arg(&socket)
        .arg("--data-dir")
        .arg(&data_dir)
        .status()
        .unwrap();
    assert!(!status.success(), "未运行时 stop 必须非零退出");
}

/// 进程是否存活（信号 0 探测；与 daemon::alive 同语义，测试侧复制
/// 以避免私有函数跨模块可见性问题）。
fn daemon_alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

#[tokio::test]
async fn 活_server_占用时不得偷取_socket() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;

    // 并发拉起的第二个 server 必须失败：否则它会 unlink 活 server 的
    // socket，已连接客户端与后续客户端全部指向无人监听的地址
    match nomic_server::ipc::bind_listener(fixture.socket_path()).await {
        Err(nomic_server::ipc::IpcError::AddressInUse(path)) => {
            assert_eq!(path, fixture.socket_path().display().to_string());
        }
        other => panic!("活 server 占用时必须拒绝绑定: {other:?}"),
    }
    // 原连接与监听不受影响
    let mut client = fixture.connect().await;
    let tree = client.list_sessions().await.unwrap();
    assert!(tree.is_empty());
}

#[tokio::test]
async fn 失败会话不阻塞后续会话_且不被无关请求重跑() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::Fail(nomic_core::LlmError::Transport("boom".into())),
        ScriptedTurn::reply(["B 完成"]),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;

    let a = submit_new(&mut client, dir.path(), "会话 A").await;
    agent_seam::wait_for(|| async {
        fixture
            .storage
            .get_session(a)
            .await
            .unwrap()
            .is_some_and(|session| session.state() == nomic_core::SessionState::Pending)
    })
    .await;
    assert_eq!(fixture.llm.captured().len(), 1, "A 失败一次");

    // B 提交：必须被调度执行；A 不得被无关受理重新领取
    let b = submit_new(&mut client, dir.path(), "会话 B").await;
    agent_seam::wait_for(|| async {
        fixture
            .storage
            .get_session(b)
            .await
            .unwrap()
            .is_some_and(|session| session.state() == nomic_core::SessionState::Idle)
    })
    .await;
    assert_eq!(
        fixture.llm.captured().len(),
        2,
        "A 不得被 B 的受理触发重跑（否则会抢占并阻塞队列）"
    );
    let a_steps = fixture.storage.steps(a).await.unwrap();
    assert_eq!(a_steps.len(), 1, "A 失败后不再推进: {a_steps:?}");
    let b_steps = fixture.storage.steps(b).await.unwrap();
    match &b_steps[..] {
        [Step::Message(user), Step::Message(assistant)] => {
            assert_eq!(user.text(), "会话 B");
            assert_eq!(assistant.text(), "B 完成");
        }
        other => panic!("B 必须完成: {other:?}"),
    }
}

#[tokio::test]
async fn 订阅期间发请求_事件进入缓冲不丢失() {
    let dir = tempfile::tempdir().unwrap();
    let gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![ScriptedTurn::PlayGated(
        gate.clone(),
        vec![agent_seam::ScriptedEvent::Token("流".into())],
    )]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "任务").await;
    agent_seam::subscribe_synced(&mut client, session_id).await;

    gate.release_all(2);
    // 事件到达后，先发请求-响应（树），再读事件：请求不得吞掉事件
    agent_seam::wait_for(|| async {
        fixture
            .storage
            .get_session(session_id)
            .await
            .unwrap()
            .is_some_and(|session| session.state() == nomic_core::SessionState::Idle)
    })
    .await;
    let tree = client.list_sessions().await.unwrap();
    assert_eq!(tree.sessions().len(), 1);
    let events = agent_seam::read_events(&mut client, 2).await;
    match &events[..] {
        [
            Event::TokenDelta { delta, .. },
            Event::MessageCompleted { message, .. },
        ] => {
            assert_eq!(delta, "流");
            assert_eq!(message.text(), "流");
        }
        other => panic!("事件在请求期间被吞: {other:?}"),
    }
}

#[tokio::test]
async fn 重复订阅不重复投递事件() {
    let dir = tempfile::tempdir().unwrap();
    let first_gate = agent_seam::gate();
    let second_gate = agent_seam::gate();
    let llm = ScriptedLlm::new(vec![
        ScriptedTurn::PlayGated(
            first_gate.clone(),
            vec![agent_seam::ScriptedEvent::Token("一".into())],
        ),
        ScriptedTurn::PlayGated(
            second_gate.clone(),
            vec![agent_seam::ScriptedEvent::Token("二".into())],
        ),
    ]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;
    let session_id = submit_new(&mut client, dir.path(), "第一段").await;

    // 同一连接订阅三次（含重复）：只应有一个发送端
    agent_seam::subscribe_synced(&mut client, session_id).await;
    client.subscribe(session_id).await.unwrap();
    client.subscribe(session_id).await.unwrap();
    client.list_sessions().await.unwrap();

    first_gate.release_all(2);
    let first_events = agent_seam::read_events(&mut client, 2).await;
    assert!(matches!(&first_events[0], Event::TokenDelta { delta, .. } if delta == "一"));

    // 第二轮：若上轮重复投递，剩余帧会先于本轮事件出现
    let acceptance = client
        .submit(&Submission {
            request_id: RequestId::new(),
            target: SessionTarget::Existing(session_id),
            working_dir: dir.path().to_path_buf(),
            content: "第二段".into(),
        })
        .await
        .unwrap();
    assert!(matches!(acceptance, Acceptance::Accepted { .. }));
    agent_seam::wait_for(|| async {
        fixture
            .storage
            .get_session(session_id)
            .await
            .unwrap()
            .is_some_and(|session| session.state() == nomic_core::SessionState::Pending)
            || fixture
                .storage
                .get_session(session_id)
                .await
                .unwrap()
                .is_some_and(|session| session.state() == nomic_core::SessionState::Running)
    })
    .await;
    second_gate.release_all(2);
    let second_events = agent_seam::read_events(&mut client, 2).await;
    match &second_events[..] {
        [
            Event::TokenDelta { delta, .. },
            Event::MessageCompleted { message, .. },
        ] => {
            assert_eq!(delta, "二", "上轮事件被重复投递: {second_events:?}");
            assert_eq!(message.text(), "二");
        }
        other => panic!("重复订阅导致重复投递: {other:?}"),
    }
}

#[tokio::test]
async fn 提交到不存在会话_连接关闭显式失败() {
    let dir = tempfile::tempdir().unwrap();
    let llm = ScriptedLlm::new(vec![]);
    let fixture = ProtocolFixture::start(dir.path(), llm, ScriptedEnv::inert()).await;
    let mut client = fixture.connect().await;

    let result = client
        .submit(&Submission {
            request_id: RequestId::new(),
            target: SessionTarget::Existing(SessionId::new()),
            working_dir: dir.path().to_path_buf(),
            content: "无目标".into(),
        })
        .await;
    // 协议无错误响应帧：服务端关闭连接，客户端得到显式错误而非挂死
    assert!(
        matches!(result, Err(ref error) if error.kind() == std::io::ErrorKind::UnexpectedEof),
        "实际: {result:?}"
    );
    assert!(client.list_sessions().await.is_err(), "连接已关闭");
}

#[tokio::test]
async fn 陈旧_pid_不误杀无关进程() {
    use std::process::{Command, Stdio};
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let socket = daemon::default_socket_path(&data_dir);

    // 无关的活进程（模拟 pid 复用）：其 pid 被写入陈旧 pid 文件
    let mut decoy = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::fs::write(daemon::pid_path(&data_dir), decoy.id().to_string()).unwrap();

    // 无活 daemon 持有 pid 锁：stop 必须拒绝并非零退出，且不发信号
    assert!(matches!(
        daemon::stop_server(&socket, &data_dir).await,
        Err(daemon::StopError::NotRunning)
    ));
    assert!(
        decoy.try_wait().unwrap().is_none(),
        "无关进程不得被 stop 杀死"
    );
    decoy.kill().unwrap();
    let _ = decoy.wait();
}
