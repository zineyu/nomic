//! 存储 seam 行为验收：提交受理与待处理工作查询。
//!
//! 覆盖受理台账（requestId 去重）、会话状态推进与调度器的原子领取；
//! 全部断言经 `nomic_core::Storage` trait 驱动临时目录上的真实
//! SQLite。

mod common;

use std::path::Path;
use std::sync::Arc;

use common::open_storage;
use nomic_core::{Message, SessionId, SessionState, Storage};
use nomic_server::storage::SQLiteStorage;

#[tokio::test]
async fn 新会话提交后进入待处理并可被原子取走() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    let submission = nomic_core::Submission {
        request_id: nomic_core::RequestId::new(),
        target: nomic_core::SessionTarget::New,
        working_dir: Path::new("/ws/project").to_path_buf(),
        content: "新任务".into(),
    };
    let accepted = storage.record_submission(&submission).await.unwrap();
    let session_id = match accepted {
        nomic_core::Acceptance::Accepted { session_id } => session_id,
        nomic_core::Acceptance::Duplicate { .. } => panic!("首次提交必须受理"),
    };

    // 提交后：消息已入会话，会话进入待处理，工作目录持久化
    let steps = storage.steps(session_id).await.unwrap();
    assert_eq!(steps.len(), 1);
    assert!(matches!(
        &steps[0],
        nomic_core::Step::Message(Message::User { text, .. }) if text == "新任务"
    ));
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Pending);
    assert_eq!(session.working_dir(), Path::new("/ws/project"));

    // 待处理工作可枚举；调度器原子领取：转为运行中，不可二次领取
    assert_eq!(
        storage
            .session_ids_in_state(SessionState::Pending)
            .await
            .unwrap(),
        vec![session_id]
    );
    assert!(storage.claim_session(session_id).await.unwrap());
    assert_eq!(
        storage
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap()
            .state(),
        SessionState::Running
    );
    assert!(!storage.claim_session(session_id).await.unwrap());

    // 步骤推进后状态随之更新
    storage
        .set_state(session_id, SessionState::Idle)
        .await
        .unwrap();
    assert_eq!(
        storage
            .get_session(session_id)
            .await
            .unwrap()
            .unwrap()
            .state(),
        SessionState::Idle
    );
    assert!(
        storage
            .session_ids_in_state(SessionState::Idle)
            .await
            .unwrap()
            .contains(&session_id)
    );
}

#[tokio::test]
async fn 向既有会话提交追加消息并置为待处理() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session = storage
        .create_session(Path::new("/ws").to_path_buf())
        .await
        .unwrap();

    let accepted = storage
        .record_submission(&nomic_core::Submission {
            request_id: nomic_core::RequestId::new(),
            target: nomic_core::SessionTarget::Existing(session.id()),
            working_dir: Path::new("/ws").to_path_buf(),
            content: "继续任务".into(),
        })
        .await
        .unwrap();
    assert!(matches!(
        accepted,
        nomic_core::Acceptance::Accepted { session_id } if session_id == session.id()
    ));
    assert_eq!(
        storage
            .get_session(session.id())
            .await
            .unwrap()
            .unwrap()
            .state(),
        SessionState::Pending
    );
    let steps = storage.steps(session.id()).await.unwrap();
    assert_eq!(steps.len(), 1);

    // 向不存在会话提交返回 NotFound，且未留下台账记录
    let missing = SessionId::new();
    let result = storage
        .record_submission(&nomic_core::Submission {
            request_id: nomic_core::RequestId::new(),
            target: nomic_core::SessionTarget::Existing(missing),
            working_dir: Path::new("/ws").to_path_buf(),
            content: "孤儿提交".into(),
        })
        .await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));
}

#[tokio::test]
async fn 同一请求_id_重复提交返回_duplicate_且消息不重复() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    let submission = nomic_core::Submission {
        request_id: nomic_core::RequestId::new(),
        target: nomic_core::SessionTarget::New,
        working_dir: Path::new("/ws/retry").to_path_buf(),
        content: "重试我".into(),
    };
    let first = storage.record_submission(&submission).await.unwrap();
    let session_id = match first {
        nomic_core::Acceptance::Accepted { session_id } => session_id,
        nomic_core::Acceptance::Duplicate { .. } => panic!("首次提交必须受理"),
    };

    // 网络重发：同一 request_id 得到同一受理结果，消息绝不重复进入会话
    let again = storage.record_submission(&submission).await.unwrap();
    assert!(matches!(
        again,
        nomic_core::Acceptance::Duplicate { session_id: dup } if dup == session_id
    ));
    assert_eq!(storage.steps(session_id).await.unwrap().len(), 1);

    // 台账持久化：重开库后重发仍被识别为重复
    drop(storage);
    let storage = SQLiteStorage::open(dir.path().join("nomic.db")).unwrap();
    let third = storage.record_submission(&submission).await.unwrap();
    assert!(matches!(
        third,
        nomic_core::Acceptance::Duplicate { session_id: dup } if dup == session_id
    ));
    assert_eq!(storage.steps(session_id).await.unwrap().len(), 1);
}

#[tokio::test]
async fn 不存在会话的状态设置返回_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    let result = storage
        .set_state(SessionId::new(), SessionState::Pending)
        .await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));
}

#[tokio::test]
async fn 不存在会话的查询返回_none_而非故障() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    // 显式缺失：确实不存在与查询故障是两种不同的返回，调用方可区分
    assert!(
        storage
            .get_session(SessionId::new())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn 待处理会话按创建顺序枚举并可逐个领取() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    let mut arrival = Vec::new();
    for content in ["先到", "第二", "第三"] {
        let accepted = storage
            .record_submission(&nomic_core::Submission {
                request_id: nomic_core::RequestId::new(),
                target: nomic_core::SessionTarget::New,
                working_dir: Path::new("/ws/queue").to_path_buf(),
                content: content.into(),
            })
            .await
            .unwrap();
        match accepted {
            nomic_core::Acceptance::Accepted { session_id } => arrival.push(session_id),
            nomic_core::Acceptance::Duplicate { .. } => panic!("首次提交必须受理"),
        }
    }

    // 只读枚举顺序等于创建顺序，且不改变会话状态
    assert_eq!(
        storage
            .session_ids_in_state(SessionState::Pending)
            .await
            .unwrap(),
        arrival
    );
    // 逐个原子领取：每个会话恰好一次成功，二次领取必失败
    for expected in &arrival {
        assert!(storage.claim_session(*expected).await.unwrap());
        assert!(!storage.claim_session(*expected).await.unwrap());
    }
    assert!(
        storage
            .session_ids_in_state(SessionState::Pending)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn 并发领取不会把同一会话分发给两个调度器() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(open_storage(dir.path()));

    let mut pending = Vec::new();
    for content in ["任务一", "任务二"] {
        let accepted = storage
            .record_submission(&nomic_core::Submission {
                request_id: nomic_core::RequestId::new(),
                target: nomic_core::SessionTarget::New,
                working_dir: Path::new("/ws").to_path_buf(),
                content: content.into(),
            })
            .await
            .unwrap();
        match accepted {
            nomic_core::Acceptance::Accepted { session_id } => pending.push(session_id),
            nomic_core::Acceptance::Duplicate { .. } => panic!("首次提交必须受理"),
        }
    }

    // 四个调度器并发领取同一批会话：原子转换保证每个会话至多一个赢家
    let mut handles = Vec::new();
    for _ in 0..4 {
        let storage = Arc::clone(&storage);
        let ids = pending.clone();
        handles.push(tokio::spawn(async move {
            let mut won = Vec::new();
            for id in ids {
                if storage.claim_session(id).await.unwrap() {
                    won.push(id);
                }
            }
            won
        }));
    }
    let mut claimed = Vec::new();
    for handle in handles {
        claimed.extend(handle.await.unwrap());
    }

    pending.sort();
    claimed.sort();
    assert_eq!(claimed, pending, "每个待处理会话恰好被一个调度器取走");
}
