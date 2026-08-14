//! SessionRecorder 集成测试：直接打在 AgentEvent 流上（内存库，无网络/外部依赖）。

use nomic_ai::{Message, Usage, UserMessage, UserMessageContent};
use nomic_core::AgentEvent;
use nomic_session::{SessionError, SessionRecorder, SessionStore};

fn user_message(text: &str, timestamp: u64) -> Message {
    Message::User(UserMessage {
        content: UserMessageContent::Text(text.to_string()),
        timestamp,
    })
}

fn message_end(message: Message) -> AgentEvent {
    AgentEvent::MessageEnd {
        message: Box::new(message),
        context_tokens: 0,
    }
}

fn compaction_end(summary: &str) -> AgentEvent {
    AgentEvent::CompactionEnd {
        summary: summary.to_string(),
        tokens_before: 12_000,
        context_tokens: 0,
        kept_count: 4,
        usage: Usage::default(),
    }
}

async fn recorder() -> (SessionRecorder, String) {
    let store = SessionStore::in_memory().await.expect("store");
    let session_id = store.create_session(".").await.expect("session");
    (SessionRecorder::new(store, session_id.clone()), session_id)
}

/// 定稿点按事件顺序落库，父指针随每次成功追加推进（父链 = 事件序）。
#[tokio::test]
async fn records_finalization_events_and_advances_tip() {
    let (mut recorder, session_id) = recorder().await;
    assert_eq!(recorder.tip(), None);

    recorder
        .record(&message_end(user_message("你好", 1)))
        .await
        .expect("record user");
    let first = recorder.tip().expect("tip advanced").to_string();

    recorder
        .record(&message_end(user_message("继续", 2)))
        .await
        .expect("record second");
    let second = recorder.tip().expect("tip advanced").to_string();
    assert_ne!(first, second);

    recorder
        .record(&compaction_end("摘要"))
        .await
        .expect("record compaction");
    let third = recorder.tip().expect("tip advanced").to_string();

    let tree = recorder
        .store()
        .list_tree(&session_id)
        .await
        .expect("list tree");
    let parent_of = |id: &str| {
        tree.iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.parent_id.clone())
    };
    assert_eq!(parent_of(&first), None);
    assert_eq!(parent_of(&second), Some(first));
    assert_eq!(parent_of(&third), Some(second.clone()));
    assert_eq!(recorder.tip(), Some(third.as_str()));
}

/// 非定稿点事件全部忽略：不落库、父指针不动。
#[tokio::test]
async fn ignores_non_finalization_events() {
    let (mut recorder, session_id) = recorder().await;
    let events = [
        AgentEvent::AgentStart,
        AgentEvent::TurnStart,
        AgentEvent::CompactionStart {
            tokens_before: 8_000,
        },
    ];
    for event in &events {
        recorder.record(event).await.expect("ignored event");
    }
    assert_eq!(recorder.tip(), None);
    assert!(
        recorder
            .store()
            .list_tree(&session_id)
            .await
            .expect("list tree")
            .is_empty()
    );
}

/// set_tip 显式切换父指针即创建分支：后续追加落在所选 entry 之下。
#[tokio::test]
async fn set_tip_branches_from_chosen_entry() {
    let (mut recorder, session_id) = recorder().await;
    recorder
        .record(&message_end(user_message("一", 1)))
        .await
        .expect("first");
    let first = recorder.tip().expect("tip").to_string();
    recorder
        .record(&message_end(user_message("二", 2)))
        .await
        .expect("second");

    recorder.set_tip(Some(first.clone()));
    recorder
        .record(&message_end(user_message("分支", 3)))
        .await
        .expect("branch append");
    let branch = recorder.tip().expect("tip").to_string();

    let tree = recorder
        .store()
        .list_tree(&session_id)
        .await
        .expect("list tree");
    let parent_of = |id: &str| {
        tree.iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.parent_id.clone())
    };
    assert_eq!(parent_of(&branch), Some(first));
}

/// 落库失败不推进父指针（下次追加重试同一父指针）。
#[tokio::test]
async fn failed_append_keeps_tip() {
    let (mut recorder, _session_id) = recorder().await;
    recorder
        .record(&message_end(user_message("一", 1)))
        .await
        .expect("first");
    let tip = recorder.tip().expect("tip").to_string();

    // 指向不存在的 entry：append 报 EntryNotFound，tip 保持不动
    recorder.set_tip(Some("nonexistent-entry".to_string()));
    let result = recorder.record(&message_end(user_message("二", 2))).await;
    assert!(matches!(result, Err(SessionError::EntryNotFound(_))));
    assert_eq!(recorder.tip(), Some("nonexistent-entry"));

    // 恢复合法父指针后可继续落库
    recorder.set_tip(Some(tip.clone()));
    recorder
        .record(&message_end(user_message("二", 2)))
        .await
        .expect("retry append");
    assert_ne!(recorder.tip(), Some(tip.as_str()));
}

/// switch 换目标 session 并重置/恢复父指针。
#[tokio::test]
async fn switch_changes_target_session_and_tip() {
    let (mut recorder, first_session) = recorder().await;
    recorder
        .record(&message_end(user_message("一", 1)))
        .await
        .expect("first");

    let second_session = recorder
        .store()
        .create_session(".")
        .await
        .expect("second session");
    recorder.switch(second_session.clone(), None);
    assert_eq!(recorder.session_id(), second_session);
    assert_eq!(recorder.tip(), None);

    recorder
        .record(&message_end(user_message("二", 2)))
        .await
        .expect("append to new session");
    assert_eq!(
        recorder
            .store()
            .list_tree(&first_session)
            .await
            .expect("first tree")
            .len(),
        1
    );
    assert_eq!(
        recorder
            .store()
            .list_tree(&second_session)
            .await
            .expect("second tree")
            .len(),
        1
    );
}
