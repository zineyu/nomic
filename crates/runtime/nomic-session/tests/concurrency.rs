//! 并发写入回归：多个 nomic 实例（各自独立的连接池）写同一 SQLite 库。

use nomic_ai::{Message, UserMessage, UserMessageContent};
use nomic_session::SessionStore;

fn user_message(text: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp,
    })
}

#[tokio::test]
async fn concurrent_writers_across_pools() {
    // WAL 下「先读后写」的 append 事务若用默认 BEGIN（deferred），升级写锁时
    // 会撞上 SQLITE_BUSY_SNAPSHOT（code 517，busy_timeout 不会重试）；
    // BEGIN IMMEDIATE 预先取写锁后应串行化而无冲突。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.db");
    let store_a = SessionStore::open(&path).await.unwrap();
    let store_b = SessionStore::open(&path).await.unwrap();
    let session = store_a.create_session(".").await.unwrap();

    let mut handles = Vec::new();
    for i in 0..200_u64 {
        let store = if i % 2 == 0 {
            store_a.clone()
        } else {
            store_b.clone()
        };
        let session = session.clone();
        handles.push(tokio::spawn(async move {
            store
                .append_message(
                    &session,
                    None,
                    &user_message(&format!("msg {i}"), 1_000 + i),
                )
                .await
        }));
    }

    let mut errors = Vec::new();
    for handle in handles {
        if let Err(error) = handle.await.expect("join task") {
            errors.push(error);
        }
    }
    assert!(
        errors.is_empty(),
        "并发写入应无冲突，实际 {} 个错误：{:?}",
        errors.len(),
        errors.first()
    );

    let messages = store_a.load_messages(&session).await.unwrap();
    assert_eq!(messages.len(), 200, "全部写入应持久化");
}
