//! 测试共享夹具与冒烟测试：最小 AppState（内存 session 库 + 空 agent
//! 构建），供 web 各子模块的测试复用。

use std::path::Path;

use clap::Parser as _;
use nomic_ai::{Model, StreamOptions};
use nomic_skills::SkillResolver;

use super::session::ResolvedSessionModel;
use super::*;

pub(super) async fn test_state() -> AppState {
    let (state, _) = test_state_with_session().await;
    state
}

/// [`test_state`] 的变体：同时返回预置 session 的 id。
pub(super) async fn test_state_with_session() -> (AppState, String) {
    let store = SessionStore::in_memory().await.expect("store");
    let id = store.create_session(".").await.expect("session");
    let models = Arc::new(ModelResolver::new(
        &Cli::parse_from(["nomic", "--model", "openai/gpt-4o"]),
        crate::settings::Settings::default(),
        None,
        None,
    ));
    let model = Model {
        id: "gpt-4o".into(),
        name: "gpt-4o".into(),
        api: nomic_ai::ApiKind::OpenAiCompletions,
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        reasoning: false,
        vision: false,
        context_window: 128_000,
        max_tokens: 4_096,
        cost_input: 0.0,
        cost_output: 0.0,
        cost_cache_read: 0.0,
        cost_cache_write: 0.0,
    };
    let provider = crate::model::build_provider(model.api, Some("sk-test".into()));
    let (events, _) = broadcast::channel::<ServerEvent>(64);
    let factory = SessionFactory {
        models: models.clone(),
        prompt_recipe: bootstrap::SystemPromptRecipe::default(),
        skill_resolver: SkillResolver::new(
            Path::new("/repo"),
            nomic_skills::ProjectDiscovery::Roots(Vec::new()),
            Vec::new(),
        )
        .expect("skills"),
        stream_options: StreamOptions::default(),
        compaction: nomic_core::CompactionSettings::default(),
        default_model: model.clone(),
        default_reasoning: None,
        available_models: vec![model.clone()],
        model_aliases: std::collections::BTreeMap::new(),
        events,
    };
    let initial = factory.build(
        Some(store.clone()),
        id.clone(),
        Vec::new(),
        std::env::current_dir().expect("cwd"),
        ResolvedSessionModel {
            model,
            provider,
            options: StreamOptions::default(),
        },
        session::SessionOpen {
            tip: None,
            membership: store.session_membership(&id).await.expect("membership"),
        },
    );
    let runtime = Arc::new(Runtime {
        store: Some(store),
        models,
        sessions: Mutex::new(HashMap::from([(id.clone(), initial)])),
        events: factory.events.clone(),
        shutdown: CancellationToken::new(),
        factory,
    });
    (AppState { inner: runtime }, id)
}

#[tokio::test]
async fn cancel_run_returns_false_when_idle() {
    let state = test_state().await;
    let session = state
        .inner
        .sessions
        .lock()
        .await
        .values()
        .next()
        .expect("session")
        .clone();
    assert!(!session.cancel_run(), "空闲时取消应返回 false");
}

#[tokio::test]
async fn answer_question_roundtrip() {
    let state = test_state().await;
    let session = state
        .inner
        .sessions
        .lock()
        .await
        .values()
        .next()
        .expect("session")
        .clone();
    let (qid, rx) = session.questions.register(AskUserQuestion {
        question: "继续？".to_string(),
        kind: nomic_tools::QuestionKind::SingleChoice,
        options: vec!["是".to_string(), "否".to_string()],
    });
    let answer = AskUserAnswer {
        answers: vec!["是".to_string()],
        custom: None,
    };
    assert!(session.answer_question(&qid, answer.clone()));
    assert_eq!(rx.await.expect("answer"), answer);
    assert!(
        !session.answer_question(&qid, answer.clone()),
        "重复回答应失败"
    );
    assert!(!session.answer_question("missing", answer));
}

#[test]
fn is_quit_key_matches_q_and_ctrl_c() {
    let key = |code, modifiers| event::KeyEvent::new(code, modifiers);
    assert!(is_quit_key(key(KeyCode::Char('q'), KeyModifiers::NONE)));
    assert!(is_quit_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    assert!(!is_quit_key(key(KeyCode::Char('c'), KeyModifiers::NONE)));
    assert!(!is_quit_key(key(KeyCode::Char('Q'), KeyModifiers::NONE)));
    assert!(!is_quit_key(key(KeyCode::Enter, KeyModifiers::NONE)));
}

#[tokio::test]
async fn snapshot_reports_state() {
    let state = test_state().await;
    let session = state
        .inner
        .sessions
        .lock()
        .await
        .values()
        .next()
        .expect("session")
        .clone();
    let snap = snapshot(&session).await.expect("snapshot");
    assert!(snap.messages.is_empty());
    assert_eq!(snap.model.provider, "openai");
    assert_eq!(snap.model.id, "gpt-4o");
    assert!(!snap.running);
    assert!(snap.queue.is_empty());
    assert!(snap.session.is_some(), "内存库 session 应存在");
    assert!(snap.pending_question.is_none());
}
