//! 存储 seam 行为验收：库打开与 schema 迁移、步骤追加与读回、
//! 任意消息分叉（写时复制）、interrupted 标记与持久化恢复。
//!
//! 全部断言经 `nomic_core::Storage` trait 驱动临时目录上的真实
//! SQLite，不断言内部 SQL 与表结构（旧版本库 fixture 的构造除外，
//! 见 `common`）。

mod common;

use std::path::Path;

use common::open_storage;
use nomic_core::{Message, MessageId, SessionId, SessionState, Storage};
use nomic_server::storage::SQLiteStorage;

/// 用 v1 旧 schema fixture 构造一个「旧版本 release 写出」的库：
/// 应用 v1 DDL、写入既有数据、把 user_version 钉在 1。
/// 返回写入的会话 id 与其消息 id。
fn build_legacy_db(path: &Path) -> (SessionId, MessageId) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(include_str!("fixtures/schema_v1.sql"))
        .unwrap();
    let session = SessionId::new();
    conn.execute(
        "INSERT INTO sessions (id, working_dir) VALUES (?1, ?2)",
        (session.to_string(), "/legacy/ws"),
    )
    .unwrap();
    let message = Message::User {
        id: MessageId::new(),
        text: "旧版本遗留消息".into(),
    };
    let message_id = message.id();
    conn.execute(
        "INSERT INTO messages (session_id, ordinal, message_id, body) VALUES (?1, 0, ?2, ?3)",
        (
            session.to_string(),
            message_id.to_string(),
            serde_json::to_string(&message).unwrap(),
        ),
    )
    .unwrap();
    // 旧 release 落用户消息时同步写步骤日志（v1 的 steps 形态）
    let step = nomic_core::Step::Message(message);
    conn.execute(
        "INSERT INTO steps (session_id, ordinal, kind, call_id, message_id, body) \
         VALUES (?1, 0, 'message', NULL, ?2, ?3)",
        (
            session.to_string(),
            message_id.to_string(),
            serde_json::to_string(&step).unwrap(),
        ),
    )
    .unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    (session, message_id)
}

#[tokio::test]
async fn 空库打开后立即可用() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    assert!(storage.session_tree().await.unwrap().is_empty());
}

#[tokio::test]
async fn 旧版本库打开后迁移且既有数据完好() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nomic.db");
    let (legacy_session, legacy_message) = build_legacy_db(&db);

    let storage = SQLiteStorage::open(&db).unwrap();
    // 迁移后旧行继续可读：state 列回填为静止，遗留消息完整读回
    let session = storage.get_session(legacy_session).await.unwrap().unwrap();
    assert_eq!(session.state(), SessionState::Idle);
    assert_eq!(session.working_dir(), Path::new("/legacy/ws"));
    let steps = storage.steps(legacy_session).await.unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(
        steps[0],
        nomic_core::Step::Message(Message::User {
            id: legacy_message,
            text: "旧版本遗留消息".into(),
        })
    );
}

#[tokio::test]
async fn 更新版本库拒绝打开() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nomic.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", 999).unwrap();
    drop(conn);

    assert!(SQLiteStorage::open(&db).is_err());
}

#[tokio::test]
async fn 不存在会话的步骤读取返回_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    let result = storage.steps(SessionId::new()).await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));
}

#[tokio::test]
async fn 追加步骤后可完整按序读回且元数据入树() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    let session = storage
        .create_session(Path::new("/ws/project").to_path_buf())
        .await
        .unwrap();
    assert_eq!(session.state(), SessionState::Idle);
    assert_eq!(session.working_dir(), Path::new("/ws/project"));

    let call_id = nomic_core::ToolCallId::new();
    let steps = vec![
        nomic_core::Step::Message(Message::User {
            id: MessageId::new(),
            text: "第一步".into(),
        }),
        nomic_core::Step::Message(Message::Assistant {
            id: MessageId::new(),
            text: "思考中".into(),
            tool_calls: vec![nomic_core::ToolCall {
                id: call_id,
                tool: nomic_core::ToolName::new("read_file"),
                arguments: serde_json::json!({"path": "a.rs"}),
            }],
        }),
        nomic_core::Step::ToolCallStarted {
            call: nomic_core::ToolCall {
                id: call_id,
                tool: nomic_core::ToolName::new("read_file"),
                arguments: serde_json::json!({"path": "a.rs"}),
            },
        },
        nomic_core::Step::ToolCallFinished {
            call_id,
            result: nomic_core::ToolResult::ok("file body"),
        },
    ];
    for step in &steps {
        storage
            .append_step(session.id(), step.clone())
            .await
            .unwrap();
    }

    // 多步追加顺序保持，完整读回
    assert_eq!(storage.steps(session.id()).await.unwrap(), steps);

    // 会话树元数据可读回
    let tree = storage.session_tree().await.unwrap();
    assert_eq!(tree.sessions(), std::slice::from_ref(&session));
    let loaded = storage.get_session(session.id()).await.unwrap().unwrap();
    assert_eq!(loaded, session);
}

#[tokio::test]
async fn 向不存在会话追加步骤返回_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    let step = nomic_core::Step::Message(Message::User {
        id: MessageId::new(),
        text: "孤儿步骤".into(),
    });
    let result = storage.append_step(SessionId::new(), step).await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));
}

#[tokio::test]
async fn 旧版本库迁移后可继续追加步骤() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nomic.db");
    let (legacy_session, _) = build_legacy_db(&db);

    let storage = SQLiteStorage::open(&db).unwrap();
    let new_message = Message::User {
        id: MessageId::new(),
        text: "迁移后续跑".into(),
    };
    storage
        .append_step(
            legacy_session,
            nomic_core::Step::Message(new_message.clone()),
        )
        .await
        .unwrap();

    let steps = storage.steps(legacy_session).await.unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[1],
        nomic_core::Step::Message(new_message),
        "迁移后追加的步骤排在遗留步骤之后"
    );
}

#[tokio::test]
async fn 任意消息处分叉父子共享此前历史且此后互不可见() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let parent = storage
        .create_session(Path::new("/ws/tree").to_path_buf())
        .await
        .unwrap();

    // 父会话三条消息：m1、m2、m3
    let mut message_ids = Vec::new();
    for text in ["m1", "m2", "m3"] {
        let message = Message::User {
            id: MessageId::new(),
            text: text.into(),
        };
        message_ids.push(message.id());
        storage
            .append_step(parent.id(), nomic_core::Step::Message(message))
            .await
            .unwrap();
    }

    // 在 m2 处分叉
    let child = storage.fork(parent.id(), message_ids[1]).await.unwrap();
    assert_eq!(child.parent().unwrap().session, parent.id());
    assert_eq!(child.parent().unwrap().forked_at, message_ids[1]);
    assert_eq!(child.working_dir(), Path::new("/ws/tree"));
    assert_eq!(child.state(), SessionState::Idle);

    // 子会话共享截至分叉点（含）的历史
    let child_steps = storage.steps(child.id()).await.unwrap();
    let child_texts: Vec<&str> = child_steps
        .iter()
        .map(|step| match step {
            nomic_core::Step::Message(m) => m.text(),
            other => panic!("分叉前缀只含消息步骤，实际: {other:?}"),
        })
        .collect();
    assert_eq!(child_texts, ["m1", "m2"]);

    // 父会话此后追加，子会话不可见
    let parent_only = Message::User {
        id: MessageId::new(),
        text: "parent only".into(),
    };
    storage
        .append_step(parent.id(), nomic_core::Step::Message(parent_only))
        .await
        .unwrap();
    let child_steps = storage.steps(child.id()).await.unwrap();
    assert_eq!(child_steps.len(), 2);

    // 子会话此后追加，父会话不可见
    let child_only = Message::User {
        id: MessageId::new(),
        text: "child only".into(),
    };
    storage
        .append_step(child.id(), nomic_core::Step::Message(child_only))
        .await
        .unwrap();
    let parent_steps = storage.steps(parent.id()).await.unwrap();
    assert_eq!(parent_steps.len(), 4);
    let child_steps = storage.steps(child.id()).await.unwrap();
    assert_eq!(child_steps.len(), 3);

    // 树形元数据：父子并存且从属关系正确
    let tree = storage.session_tree().await.unwrap();
    assert_eq!(tree.len(), 2);
}

#[tokio::test]
async fn 分叉点不属于来源会话返回_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let parent = storage
        .create_session(Path::new("/ws").to_path_buf())
        .await
        .unwrap();

    // 来源会话不存在
    let result = storage.fork(SessionId::new(), MessageId::new()).await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));

    // 分叉点不属于来源会话
    let result = storage.fork(parent.id(), MessageId::new()).await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));

    // 失败后没有留下幽灵会话
    let tree = storage.session_tree().await.unwrap();
    assert_eq!(tree.sessions(), std::slice::from_ref(&parent));
}

#[tokio::test]
async fn 分叉前缀中的消息步骤保持原序即使穿插工具步骤() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let parent = storage
        .create_session(Path::new("/ws/fork").to_path_buf())
        .await
        .unwrap();

    let call = nomic_core::ToolCall {
        id: nomic_core::ToolCallId::new(),
        tool: nomic_core::ToolName::new("read_file"),
        arguments: serde_json::json!({"path": "a.rs"}),
    };
    let m1 = Message::User {
        id: MessageId::new(),
        text: "m1".into(),
    };
    let assistant = Message::Assistant {
        id: MessageId::new(),
        text: String::new(),
        tool_calls: vec![call.clone()],
    };
    let tool_reply = Message::Tool {
        id: MessageId::new(),
        call_id: call.id,
        result: nomic_core::ToolResult::ok("file body"),
    };
    let m2 = Message::User {
        id: MessageId::new(),
        text: "m2".into(),
    };
    let m3 = Message::User {
        id: MessageId::new(),
        text: "m3".into(),
    };
    for step in [
        nomic_core::Step::Message(m1.clone()),
        nomic_core::Step::Message(assistant.clone()),
        nomic_core::Step::ToolCallStarted { call: call.clone() },
        nomic_core::Step::ToolCallFinished {
            call_id: call.id,
            result: nomic_core::ToolResult::ok("file body"),
        },
        nomic_core::Step::Message(tool_reply.clone()),
        nomic_core::Step::Message(m2.clone()),
        nomic_core::Step::Message(m3.clone()),
    ] {
        storage.append_step(parent.id(), step).await.unwrap();
    }

    // 分叉复制消息前缀：穿插的工具步骤不属于前缀；分叉点之后的消息（m3）不复制
    let child = storage.fork(parent.id(), m2.id()).await.unwrap();
    assert_eq!(
        storage.steps(child.id()).await.unwrap(),
        vec![
            nomic_core::Step::Message(m1),
            nomic_core::Step::Message(assistant),
            nomic_core::Step::Message(tool_reply),
            nomic_core::Step::Message(m2),
        ]
    );
    assert_eq!(storage.steps(parent.id()).await.unwrap().len(), 7);
}

#[tokio::test]
async fn 未完成工具调用可标记_interrupted_并携带部分输出() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session = storage
        .create_session(Path::new("/ws").to_path_buf())
        .await
        .unwrap();

    let call = nomic_core::ToolCall {
        id: nomic_core::ToolCallId::new(),
        tool: nomic_core::ToolName::new("run_tests"),
        arguments: serde_json::json!({}),
    };
    storage
        .append_step(
            session.id(),
            nomic_core::Step::ToolCallStarted { call: call.clone() },
        )
        .await
        .unwrap();

    // 崩溃恢复路径：未完成的调用标记 interrupted，已提交的部分输出随步骤落库
    storage
        .mark_interrupted(session.id(), call.id, Some("3 passed, 2 running".into()))
        .await
        .unwrap();

    let steps = storage.steps(session.id()).await.unwrap();
    assert_eq!(
        steps,
        vec![
            nomic_core::Step::ToolCallStarted { call: call.clone() },
            nomic_core::Step::ToolCallInterrupted {
                call_id: call.id,
                partial_output: Some("3 passed, 2 running".into()),
            },
        ]
    );

    // 无任何已提交输出时 partial_output 为 None
    let call2 = nomic_core::ToolCall {
        id: nomic_core::ToolCallId::new(),
        tool: nomic_core::ToolName::new("run_tests"),
        arguments: serde_json::json!({}),
    };
    storage
        .append_step(
            session.id(),
            nomic_core::Step::ToolCallStarted {
                call: call2.clone(),
            },
        )
        .await
        .unwrap();
    storage
        .mark_interrupted(session.id(), call2.id, None)
        .await
        .unwrap();
    let steps = storage.steps(session.id()).await.unwrap();
    assert_eq!(
        steps[3],
        nomic_core::Step::ToolCallInterrupted {
            call_id: call2.id,
            partial_output: None,
        },
        "部分输出缺省为 None"
    );
}

#[tokio::test]
async fn 非法_interrupted_标记返回_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let session = storage
        .create_session(Path::new("/ws").to_path_buf())
        .await
        .unwrap();
    let call = nomic_core::ToolCall {
        id: nomic_core::ToolCallId::new(),
        tool: nomic_core::ToolName::new("run_tests"),
        arguments: serde_json::json!({}),
    };
    storage
        .append_step(
            session.id(),
            nomic_core::Step::ToolCallStarted { call: call.clone() },
        )
        .await
        .unwrap();

    // 会话不存在
    let result = storage
        .mark_interrupted(SessionId::new(), call.id, None)
        .await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));

    // 调用不存在
    let result = storage
        .mark_interrupted(session.id(), nomic_core::ToolCallId::new(), None)
        .await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));

    // 已完成的调用不可再标记
    storage
        .append_step(
            session.id(),
            nomic_core::Step::ToolCallFinished {
                call_id: call.id,
                result: nomic_core::ToolResult::ok("done"),
            },
        )
        .await
        .unwrap();
    let result = storage
        .mark_interrupted(session.id(), call.id, Some("late".into()))
        .await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));

    // 已标记 interrupted 的调用也不可重复标记
    let call2 = nomic_core::ToolCall {
        id: nomic_core::ToolCallId::new(),
        tool: nomic_core::ToolName::new("run_tests"),
        arguments: serde_json::json!({}),
    };
    storage
        .append_step(
            session.id(),
            nomic_core::Step::ToolCallStarted {
                call: call2.clone(),
            },
        )
        .await
        .unwrap();
    storage
        .mark_interrupted(session.id(), call2.id, None)
        .await
        .unwrap();
    let result = storage.mark_interrupted(session.id(), call2.id, None).await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));

    // 失败后步骤序列不被污染
    assert_eq!(storage.steps(session.id()).await.unwrap().len(), 4);
}

#[tokio::test]
async fn 分叉树与中断标记经重开完整恢复() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("nomic.db");

    let (parent_id, fork_point, child_id, grandchild_id, call_id);
    {
        let storage = SQLiteStorage::open(&db).unwrap();
        let parent = storage
            .create_session(Path::new("/ws/recover").to_path_buf())
            .await
            .unwrap();
        parent_id = parent.id();
        let m1 = Message::User {
            id: MessageId::new(),
            text: "起点".into(),
        };
        fork_point = m1.id();
        storage
            .append_step(parent_id, nomic_core::Step::Message(m1))
            .await
            .unwrap();

        let child = storage.fork(parent_id, fork_point).await.unwrap();
        child_id = child.id();
        let call = nomic_core::ToolCall {
            id: nomic_core::ToolCallId::new(),
            tool: nomic_core::ToolName::new("run_shell"),
            arguments: serde_json::json!({"cmd": "cargo test"}),
        };
        call_id = call.id;
        storage
            .append_step(child_id, nomic_core::Step::ToolCallStarted { call })
            .await
            .unwrap();
        storage
            .mark_interrupted(child_id, call_id, Some("partial log".into()))
            .await
            .unwrap();

        // 父会话分叉后继续追加，子会话不可见
        storage
            .append_step(
                parent_id,
                nomic_core::Step::Message(Message::User {
                    id: MessageId::new(),
                    text: "父继续".into(),
                }),
            )
            .await
            .unwrap();
        // 子会话再分叉出孙会话，树形从属成链
        let grandchild = storage.fork(child_id, fork_point).await.unwrap();
        grandchild_id = grandchild.id();
        assert_eq!(grandchild.parent().unwrap().session, child_id);

        // 既有会话受理新提交后保持待处理，跨重开可被调度器取走
        storage
            .record_submission(&nomic_core::Submission {
                request_id: nomic_core::RequestId::new(),
                target: nomic_core::SessionTarget::Existing(parent_id),
                working_dir: Path::new("/ws/recover").to_path_buf(),
                content: "恢复后续跑".into(),
            })
            .await
            .unwrap();
    }

    // 重开同一库：结构、步骤与状态全部从持久化事实重建
    let storage = SQLiteStorage::open(&db).unwrap();
    assert_eq!(storage.session_tree().await.unwrap().len(), 3);

    let parent = storage.get_session(parent_id).await.unwrap().unwrap();
    assert_eq!(parent.parent(), None);
    assert_eq!(parent.state(), SessionState::Pending);
    let child = storage.get_session(child_id).await.unwrap().unwrap();
    assert_eq!(
        child.parent(),
        Some(nomic_core::ParentRef {
            session: parent_id,
            forked_at: fork_point,
        })
    );
    assert_eq!(child.state(), SessionState::Idle);
    let grandchild = storage.get_session(grandchild_id).await.unwrap().unwrap();
    assert_eq!(
        grandchild.parent(),
        Some(nomic_core::ParentRef {
            session: child_id,
            forked_at: fork_point,
        })
    );

    let parent_steps = storage.steps(parent_id).await.unwrap();
    assert_eq!(parent_steps.len(), 3);
    assert!(matches!(&parent_steps[1], nomic_core::Step::Message(m) if m.text() == "父继续"));
    assert!(matches!(&parent_steps[2], nomic_core::Step::Message(m) if m.text() == "恢复后续跑"));
    let child_steps = storage.steps(child_id).await.unwrap();
    assert_eq!(child_steps.len(), 3);
    assert!(matches!(&child_steps[0], nomic_core::Step::Message(m) if m.text() == "起点"));
    assert!(
        matches!(
            &child_steps[2],
            nomic_core::Step::ToolCallInterrupted {
                call_id: got,
                partial_output: Some(output),
            } if *got == call_id && output == "partial log"
        ),
        "中断步骤携带的部分输出必须随库持久化"
    );
    assert_eq!(storage.claim_next_pending().await.unwrap(), Some(parent_id));
}

#[tokio::test]
async fn 删除会话_级联删除其子树_无关会话不受影响() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());

    // 父会话 + 一条用户消息
    let parent = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();
    storage
        .record_submission(&nomic_core::Submission {
            request_id: nomic_core::RequestId::new(),
            target: nomic_core::SessionTarget::Existing(parent.id()),
            working_dir: dir.path().to_path_buf(),
            content: "父消息".into(),
        })
        .await
        .unwrap();
    // 分叉出子会话（复制父的消息前缀）
    let fork_point = {
        let steps = storage.steps(parent.id()).await.unwrap();
        match &steps[0] {
            nomic_core::Step::Message(message) => message.id(),
            other => panic!("首步骤必须是消息: {other:?}"),
        }
    };
    let child = storage.fork(parent.id(), fork_point).await.unwrap();
    // 无关会话
    let other = storage
        .create_session(dir.path().to_path_buf())
        .await
        .unwrap();

    storage.delete(parent.id()).await.unwrap();

    // 父与子一并消失：元数据与步骤都不可再读
    assert_eq!(storage.get_session(parent.id()).await.unwrap(), None);
    assert_eq!(storage.get_session(child.id()).await.unwrap(), None);
    assert!(matches!(
        storage.steps(parent.id()).await,
        Err(nomic_core::StorageError::NotFound(_))
    ));
    assert!(matches!(
        storage.steps(child.id()).await,
        Err(nomic_core::StorageError::NotFound(_))
    ));
    // 无关会话原样保留
    let tree = storage.session_tree().await.unwrap();
    assert_eq!(tree.sessions(), std::slice::from_ref(&other));
    assert_eq!(storage.steps(other.id()).await.unwrap(), Vec::new());
}

#[tokio::test]
async fn 删除不存在会话返回_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let storage = open_storage(dir.path());
    let result = storage.delete(SessionId::new()).await;
    assert!(matches!(result, Err(nomic_core::StorageError::NotFound(_))));
}
