//! 测试共享夹具与冒烟测试：最小 WebState（内存 session 库 + 空 agent
//! 构建），供 web 各子模块的测试复用。

use std::path::Path;

use clap::Parser as _;
use nomic_ai::{Model, StreamOptions};
use nomic_skills::SkillResolver;

use super::session::ResolvedSessionModel;
use super::*;
use crate::model::ModelResolver;
use nomic_session::SessionStore;

pub(super) async fn test_state() -> WebState {
    let (state, _) = test_state_with_session().await;
    state
}

/// [`test_state`] 的变体：同时返回预置 session 的 id。
pub(super) async fn test_state_with_session() -> (WebState, String) {
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
    // 进程级服务状态（ADR-0047）：测试夹具逐字段注册最小服务集
    let services = AppState::new(crate::state::Services {
        model: model.clone(),
        model_configured: true,
        models,
        provider: provider.clone(),
        stream_options: StreamOptions::default(),
        system_prompt: String::new(),
        compaction: nomic_core::CompactionSettings::default(),
        store: Some(store.clone()),
        session: Some((store.clone(), id.clone())),
        project: std::env::current_dir().expect("cwd"),
        history: Vec::new(),
        prompt_recipe: bootstrap::SystemPromptRecipe::default(),
        skill_resolver: SkillResolver::new(
            Path::new("/repo"),
            nomic_skills::ProjectDiscovery::Roots(Vec::new()),
            Vec::new(),
        )
        .expect("skills"),
        prompt_templates: Vec::new(),
        available_models: vec![model.clone()],
        model_aliases: std::collections::BTreeMap::new(),
        bus: crate::state::EventBus::new(),
    });
    let factory = SessionFactory {
        state: services.clone(),
        default_reasoning: None,
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
        services,
        sessions: Mutex::new(HashMap::from([(id.clone(), initial)])),
        shutdown: CancellationToken::new(),
        factory,
    });
    (WebState { inner: runtime }, id)
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
