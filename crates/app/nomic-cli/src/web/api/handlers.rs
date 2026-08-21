//! WebSocket 事件 handler：查询类（`get_state` / `list_models` / `list_sessions` /
//! `list_workspaces`）与命令类（`prompt` / `cancel` / `answer_question` /
//! `switch_model` / `create_session` / `create_workspace`）的具体实现，以及
//! 共享类型与辅助函数。

use std::sync::Arc;

use nomic_ai::{Message, Model, ThinkingLevel};
use nomic_tools::{AskUserAnswer, AskUserQuestion};
use serde::Serialize;

use super::ApiError;
use crate::model::ModelChoice;
use crate::web::{AppState, ServerEvent, Snapshot};

// ── 查询类 handler（返回带 request_id 的 ServerEvent）─────────────────────

/// 获取会话快照：消息历史、模型、思考级别、运行状态等。
pub async fn handle_get_state(state: &AppState, session_id: &str, request_id: &str) -> ServerEvent {
    let result = async {
        let session = open_session(state, session_id).await?;
        let snapshot = crate::web::snapshot(&session).await?;
        Ok::<_, ApiError>(snapshot)
    }
    .await;
    match result {
        Ok(snapshot) => ServerEvent::StateSnapshot {
            session_id: session_id.to_string(),
            request_id: request_id.to_string(),
            snapshot: Box::new(SnapshotView::from_snapshot(snapshot)),
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 候选模型列表（跨 provider；当前选择由会话快照携带）。
pub fn handle_list_models(state: &AppState, request_id: &str) -> ServerEvent {
    let default_model = state.inner.factory.default_model.clone();
    let current = crate::model::ModelSelection {
        provider: default_model.provider,
        model: default_model.id,
    };
    let candidates = state.inner.models.candidates(&current);
    ServerEvent::ModelsList {
        request_id: request_id.to_string(),
        candidates,
    }
}

/// 列出全部 session 摘要。
pub async fn handle_list_sessions(state: &AppState, request_id: &str) -> ServerEvent {
    match state.inner.list_sessions().await {
        Ok(sessions) => ServerEvent::SessionsList {
            request_id: request_id.to_string(),
            sessions,
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// 列出全部 workspace 摘要。
pub async fn handle_list_workspaces(state: &AppState, request_id: &str) -> ServerEvent {
    match state.inner.list_workspaces().await {
        Ok(workspaces) => ServerEvent::WorkspacesList {
            request_id: request_id.to_string(),
            workspaces,
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

/// skill 清单（`@skill:` 补全用；进程级 skill 解析器快照，与 TUI 补全同一来源）。
pub fn handle_list_skills(state: &AppState, request_id: &str) -> ServerEvent {
    let skills = state
        .inner
        .factory
        .skill_resolver
        .catalog()
        .into_iter()
        .map(|skill| SkillItem {
            name: skill.name,
            description: skill.document.description,
        })
        .collect();
    ServerEvent::SkillsList {
        request_id: request_id.to_string(),
        skills,
    }
}

/// 文件候选（`@file:` 补全用；相对目标 session 的 workspace 前缀匹配）。
/// 最多返回 [`MAX_FILE_CANDIDATES`] 条，避免大目录撑爆事件负载。
pub async fn handle_list_files(
    state: &AppState,
    session_id: &str,
    prefix: &str,
    request_id: &str,
) -> ServerEvent {
    let session = match open_session(state, session_id).await {
        Ok(session) => session,
        Err(error) => return error.to_ws_response(Some(request_id)),
    };
    let mut files = crate::mention::file_mention_candidates(prefix, &session.workspace);
    files.truncate(MAX_FILE_CANDIDATES);
    ServerEvent::FilesList {
        request_id: request_id.to_string(),
        files,
    }
}

/// `@file:` 补全候选的返回上限（大目录如 `target/` 单层也有数百文件）。
const MAX_FILE_CANDIDATES: usize = 100;

// ── 命令类 handler（返回 ack ServerEvent）─────────────────────────────────

/// 提交 prompt（空闲即跑，运行中入队）；返回 ack 携带排队状态。
///
/// `/` 开头的输入按斜杠命令解析（`/compact [聚焦指令]`、`/continue`），
/// 与 prompt 共用同一队列串行执行（core runner，ADR-0033）；其余文本
/// 在提交前展开有效 `@skill:` / `@file:` mention（相对本 session 的
/// workspace），无效标记原样保留——与 TUI 同一口径。
pub async fn handle_prompt(
    state: &AppState,
    session_id: &str,
    text: String,
    images: Vec<nomic_ai::ImageContent>,
) -> ServerEvent {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ApiError::BadRequest("prompt 为空".to_string()).to_ws_response(None);
    }
    let session = match open_session(state, session_id).await {
        Ok(s) => s,
        Err(error) => return error.to_ws_response(None),
    };
    let job = if let Some(rest) = trimmed.strip_prefix('/') {
        match parse_slash_command(rest) {
            Ok(job) => job,
            Err(error) => return error.to_ws_response(None),
        }
    } else {
        // 发送前展开有效 `@skill:` / `@file:` mention；无效标记原样保留
        let expanded = crate::mention::expand_mentions(
            trimmed,
            &state.inner.factory.skill_resolver,
            &session.workspace,
        );
        nomic_core::SessionJob::Prompt {
            text: expanded,
            images,
        }
    };
    // runner 串行消费（提交时已在运行则排队）；提交前读运行态作 ack
    let queued = session.runner.is_running();
    if let Err(error) = session.runner.submit(job) {
        return ApiError::Internal(error.to_string()).to_ws_response(None);
    }
    ServerEvent::PromptAck {
        session_id: session_id.to_string(),
        queued,
    }
}

/// 解析斜杠命令体（已去掉前导 `/`）；未知命令或参数非法时返回带用法
/// 提示的错误。web 支持的命令子集：`/compact [聚焦指令]`、`/continue`。
fn parse_slash_command(rest: &str) -> Result<nomic_core::SessionJob, ApiError> {
    const USAGE: &str = "可用命令：/compact [聚焦指令]（压缩上下文）、/continue（续跑上次运行）";
    // 命令名取到首个 `:` 或空白为止；其余部分为参数（`compact 指令` 与
    // `compact:指令` 两种形式等价，冒号形式与 TUI 命令语法对齐）
    let (name, arg) = match rest.find(|c: char| c == ':' || c.is_whitespace()) {
        Some(index) => {
            let (name, tail) = rest.split_at(index);
            let delimiter = tail.chars().next().expect("find 命中必有字符");
            (
                name,
                Some(tail[delimiter.len_utf8()..].trim()).filter(|arg| !arg.is_empty()),
            )
        }
        None => (rest, None),
    };
    match name {
        "compact" => Ok(nomic_core::SessionJob::Compact {
            instructions: arg.map(str::to_string),
        }),
        "continue" if arg.is_none() => Ok(nomic_core::SessionJob::Continue),
        _ => Err(ApiError::BadRequest(format!("未知命令 /{rest}。{USAGE}"))),
    }
}

/// 取消当前轮运行。
pub async fn handle_cancel(state: &AppState, session_id: &str) -> ServerEvent {
    let session = match open_session(state, session_id).await {
        Ok(s) => s,
        Err(error) => return error.to_ws_response(None),
    };
    session.cancel_run();
    ServerEvent::CancelAck {
        session_id: session_id.to_string(),
    }
}

/// 回答提问：经注册表回填给等待中的工具。
pub async fn handle_answer_question(
    state: &AppState,
    session_id: &str,
    qid: String,
    answers: Vec<String>,
    custom: Option<String>,
) -> ServerEvent {
    let session = match open_session(state, session_id).await {
        Ok(s) => s,
        Err(error) => return error.to_ws_response(None),
    };
    let answer = AskUserAnswer { answers, custom };
    if session.answer_question(&qid, answer) {
        ServerEvent::AnswerAck {
            session_id: session_id.to_string(),
        }
    } else {
        ApiError::NotFound(format!("question {qid} 不存在或已被回答")).to_ws_response(None)
    }
}

/// 切换会话模型；结果落库到会话级 config。
pub async fn handle_switch_model(
    state: &AppState,
    session_id: &str,
    spec: String,
    reasoning: Option<String>,
) -> ServerEvent {
    let session = match open_session(state, session_id).await {
        Ok(s) => s,
        Err(error) => return error.to_ws_response(None),
    };
    let current = match session.handle.model().await {
        Ok(m) => m,
        Err(error) => {
            return ApiError::from(error).to_ws_response(None);
        }
    };
    let selection = match crate::model::ModelSelection::parse(&spec, Some(&current.provider)) {
        Ok(s) => s,
        Err(error) => {
            return ApiError::BadRequest(format!("{error:#}")).to_ws_response(None);
        }
    };
    let model = match state
        .inner
        .models
        .resolve(&selection.provider, &selection.model)
    {
        Ok(m) => m,
        Err(error) => {
            return ApiError::BadRequest(format!("{error:#}")).to_ws_response(None);
        }
    };

    if model.provider != current.provider {
        let api_key = crate::model::resolve_api_key(
            None,
            std::env::var(crate::model::api_key_env(model.api))
                .ok()
                .as_deref(),
            state
                .inner
                .models
                .provider_config(&model.provider)
                .and_then(|p| p.api_key.as_deref()),
            state
                .inner
                .models
                .config()
                .and_then(|c| c.api_key.as_deref()),
        );
        if session
            .handle
            .set_provider(
                crate::model::build_provider(model.api, api_key.clone()),
                api_key,
            )
            .is_err()
        {
            return ApiError::Internal("agent actor 已退出".to_string()).to_ws_response(None);
        }
    }
    if session.handle.set_model(model.clone()).is_err() {
        return ApiError::Internal("agent actor 已退出".to_string()).to_ws_response(None);
    }

    if let Some(level) = reasoning.as_deref() {
        match parse_thinking_level(level) {
            Ok(level) => {
                let _ = session.handle.set_reasoning(level);
                persist_session_reasoning(state, session_id, level).await;
            }
            Err(error) => return error.to_ws_response(None),
        }
    }

    // 选择落库（会话级 config，与 TUI 同 append-only 口径）；失败仅告警
    persist_session_model(state, session_id, &selection.spec()).await;

    ServerEvent::SwitchModelAck {
        session_id: session_id.to_string(),
        choice: ModelChoice {
            provider: model.provider,
            id: model.id,
            name: model.name,
            context_window: model.context_window,
            reasoning: model.reasoning,
        },
    }
}

/// 新建 session（新对话语义，默认模型）；必须指定归属目录 `workspace`
/// （无默认 workspace；目录不存在或不是目录时拒绝，不静默登记无效路径）。
pub async fn handle_create_session(state: &AppState, workspace: String) -> ServerEvent {
    let workspace = match expand_workspace_dir(&workspace) {
        Ok(workspace) => workspace,
        Err(error) => return error.to_ws_response(None),
    };
    match state.inner.create_session(&workspace).await {
        Ok(session) => ServerEvent::SessionCreated {
            id: session.id.clone(),
            title: None,
        },
        Err(error) => error.to_ws_response(None),
    }
}

/// 登记新 workspace（按路径查或插，幂等）；响应携带 `request_id` 供客户端关联。
pub async fn handle_create_workspace(
    state: &AppState,
    request_id: &str,
    path: String,
) -> ServerEvent {
    let path = match expand_workspace_dir(&path) {
        Ok(path) => path,
        Err(error) => return error.to_ws_response(Some(request_id)),
    };
    match state.inner.create_workspace(&path).await {
        Ok(workspace) => ServerEvent::WorkspaceCreated {
            request_id: request_id.to_string(),
            id: workspace.id,
            path: workspace.path.display().to_string(),
        },
        Err(error) => error.to_ws_response(Some(request_id)),
    }
}

// ── 共享类型 ──────────────────────────────────────────────────────────────

/// skill 清单条目（`list_skills` 响应；`@skill:` 补全弹层展示用）。
#[derive(Debug, Clone, Serialize)]
pub struct SkillItem {
    pub name: String,
    pub description: String,
}

/// 会话快照视图（WebSocket 响应携带，前端用于初始化/刷新状态）。
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotView {
    pub messages: Vec<Message>,
    pub model: Model,
    pub reasoning: Option<ThinkingLevel>,
    pub context_tokens: u64,
    pub running: bool,
    pub queued: usize,
    pub session: Option<(String, Option<String>)>,
    pub pending_question: Option<(String, AskUserQuestion)>,
    /// 本 session 的 workspace 路径（操作基准）
    pub workspace: String,
    /// 会话统计信息（前端状态栏展示用）
    #[serde(flatten)]
    pub stats: nomic_core::SessionStats,
}

impl SnapshotView {
    fn from_snapshot(snap: Snapshot) -> Self {
        Self {
            messages: snap.messages,
            model: snap.model,
            reasoning: snap.reasoning,
            context_tokens: snap.context_tokens,
            running: snap.running,
            queued: snap.queued,
            session: snap.session,
            pending_question: snap.pending_question,
            workspace: snap.workspace.display().to_string(),
            stats: snap.stats,
        }
    }
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────

/// 展开用户输入的 workspace 目录：去空白、`~/` 展开为家目录。
/// 空白输入返回 `BadRequest`；目录存在性由 `Runtime` 层校验。
fn expand_workspace_dir(input: &str) -> Result<std::path::PathBuf, ApiError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest("workspace 目录为空".to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        let home = dirs::home_dir()
            .ok_or_else(|| ApiError::Internal("无法定位家目录（~ 展开失败）".to_string()))?;
        return Ok(home.join(rest));
    }
    Ok(std::path::PathBuf::from(trimmed))
}

/// 从运行时打开（或惰性构建）指定 session。
async fn open_session(
    state: &AppState,
    id: &str,
) -> Result<Arc<crate::web::SessionRuntime>, ApiError> {
    state.inner.open_session(id).await
}

/// 会话级模型选择落库；库不可用或写失败仅告警不阻断切换。
async fn persist_session_model(state: &AppState, session_id: &str, spec: &str) {
    let Some(store) = &state.inner.store else {
        return;
    };
    if let Err(error) = store
        .set_session_config(
            session_id,
            crate::model::CONFIG_KEY_MODEL,
            &serde_json::Value::String(spec.to_string()),
        )
        .await
    {
        tracing::warn!(%error, "会话级模型选择落库失败");
    }
}

/// 会话级思考级别落库；库不可用或写失败仅告警不阻断切换。
async fn persist_session_reasoning(
    state: &AppState,
    session_id: &str,
    level: Option<ThinkingLevel>,
) {
    let Some(store) = &state.inner.store else {
        return;
    };
    let value = level.map_or("off", ThinkingLevel::as_str);
    if let Err(error) = store
        .set_session_config(
            session_id,
            crate::model::CONFIG_KEY_REASONING,
            &serde_json::Value::String(value.to_string()),
        )
        .await
    {
        tracing::warn!(%error, "会话级思考级别落库失败");
    }
}

/// 解析思考级别请求值；`off` → `None`（关闭）。
fn parse_thinking_level(level: &str) -> Result<Option<ThinkingLevel>, ApiError> {
    ThinkingLevel::parse_setting(level).map_err(|error| ApiError::BadRequest(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_slash_command_compact_and_continue() {
        assert!(matches!(
            parse_slash_command("compact").expect("compact"),
            nomic_core::SessionJob::Compact { instructions: None }
        ));
        // 自由文本指令（空格形式）
        let Ok(nomic_core::SessionJob::Compact {
            instructions: Some(instructions),
        }) = parse_slash_command("compact 专注 测试 部分")
        else {
            panic!("compact 带指令应解析成功");
        };
        assert_eq!(instructions, "专注 测试 部分");
        // 冒号形式（与 TUI 命令语法对齐）
        let Ok(nomic_core::SessionJob::Compact {
            instructions: Some(instructions),
        }) = parse_slash_command("compact:focus on tests")
        else {
            panic!("compact:指令 应解析成功");
        };
        assert_eq!(instructions, "focus on tests");
        assert!(matches!(
            parse_slash_command("continue").expect("continue"),
            nomic_core::SessionJob::Continue
        ));
    }

    #[test]
    fn parse_slash_command_rejects_unknown_and_invalid_usage() {
        let Err(ApiError::BadRequest(message)) = parse_slash_command("quit") else {
            panic!("未知命令应报错");
        };
        assert!(message.contains("/compact"), "{message}");
        assert!(message.contains("/continue"), "{message}");
        // continue 不接受参数
        assert!(parse_slash_command("continue extra").is_err());
        // 空命令名
        assert!(parse_slash_command("").is_err());
    }

    /// `list_files` 以目标 session 的 workspace 为基准做前缀匹配（测试
    /// session 的 workspace 是 crate 根目录）。
    #[tokio::test]
    async fn list_files_matches_prefix_under_session_workspace() {
        let (state, session_id) = crate::web::tests::test_state_with_session().await;

        let event = handle_list_files(&state, &session_id, "src/mai", "r1").await;
        let ServerEvent::FilesList { request_id, files } = event else {
            panic!("应返回 FilesList");
        };
        assert_eq!(request_id, "r1");
        assert!(files.contains(&"src/main.rs".to_string()), "{files:?}");

        // 未命中前缀返回空列表
        let event = handle_list_files(&state, &session_id, "src/no-such-file", "r2").await;
        let ServerEvent::FilesList { files, .. } = event else {
            panic!("应返回 FilesList");
        };
        assert!(files.is_empty(), "{files:?}");
    }

    /// `list_skills` 返回进程级 skill 清单（测试环境无 skill，为空列表）。
    #[tokio::test]
    async fn list_skills_roundtrip() {
        let state = crate::web::tests::test_state().await;
        let event = handle_list_skills(&state, "r3");
        let ServerEvent::SkillsList { request_id, skills } = event else {
            panic!("应返回 SkillsList");
        };
        assert_eq!(request_id, "r3");
        assert!(skills.is_empty());
    }

    /// `/` 命令与 prompt 的串行队列语义收在 core runner（集成测试覆盖）；
    /// web 侧只验证提交路径：空闲时提交的 ack 不标记排队。
    #[tokio::test]
    async fn prompt_ack_not_queued_when_idle() {
        let (state, session_id) = crate::web::tests::test_state_with_session().await;
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .get(&session_id)
            .expect("session")
            .clone();
        assert!(!session.runner.is_running(), "预置 session 应空闲");

        // 斜杠命令同样走 runner 队列（/continue 空历史立即结束，不发起请求）
        let ack = handle_prompt(&state, &session_id, "/continue".to_string(), Vec::new()).await;
        let ServerEvent::PromptAck { queued, .. } = ack else {
            panic!("应返回 PromptAck");
        };
        assert!(!queued, "空闲时提交不应标记排队");
    }

    /// 未知斜杠命令不应进入队列，直接回错误事件。
    #[tokio::test]
    async fn unknown_slash_command_is_rejected() {
        let (state, session_id) = crate::web::tests::test_state_with_session().await;
        let event = handle_prompt(&state, &session_id, "/quit".to_string(), Vec::new()).await;
        let ServerEvent::Error { message, .. } = event else {
            panic!("未知命令应返回 error 事件");
        };
        assert!(message.contains("未知命令"), "{message}");
        let session = state
            .inner
            .sessions
            .lock()
            .await
            .get(&session_id)
            .expect("session")
            .clone();
        assert_eq!(session.runner.queued_len(), 0, "未知命令不应入队");
    }
}
