//! 会话管理：`resume` 恢复、`tree` 浏览与分支、`new` 新建。
//!
//! 定稿点落库与父指针推进收在 `nomic_session::SessionRecorder`；本模块
//! 持有会话落库绑定 [`SessionBinding`]（recorder + 与工具共享的基准目录
//! 句柄），driver 只持有一个实例并转发——事件分支的落库是
//! `session.record(&event)` 一行，session 切换时的 recorder 换绑与基准
//! 切换收在本模块的 effect 函数里。

use anyhow::{Context as _, Result};
use nomic_core::{AgentEvent, AgentHandle};
use nomic_session::{SessionError, SessionRecorder, SessionStore, TreeEntry};
use nomic_tools::BaseDir;

use crate::tui::app::{App, PickerRow};

/// 会话落库绑定：recorder（store、目标 session、父指针）与当前 session 的
/// 操作基准（workspace 严格归属；与工具共享同一句柄，`set` 后下一次工具
/// 执行即用新基准）。`None` recorder 表示本次不持久化。
pub(in crate::tui) struct SessionBinding {
    recorder: Option<SessionRecorder>,
    base: BaseDir,
}

impl SessionBinding {
    pub(in crate::tui) const fn new(recorder: Option<SessionRecorder>, base: BaseDir) -> Self {
        Self { recorder, base }
    }

    /// 可用的 session store（模型选择落库等跨关注点读取用）；未持久化时为 `None`
    pub(in crate::tui) fn store(&self) -> Option<SessionStore> {
        self.recorder
            .as_ref()
            .map(|recorder| recorder.store().clone())
    }

    /// 当前 session 的操作基准（mention 文件路径解析、新建 session 的
    /// workspace 归属以它为基准）；句柄未设置时退回进程 cwd。
    pub(in crate::tui) fn base_dir(&self) -> std::path::PathBuf {
        self.base.snapshot().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        })
    }

    /// 事件落库：定稿点落库与父指针推进（与 print 同一实现）；未持久化时无操作。
    pub(in crate::tui) async fn record(&mut self, event: &AgentEvent) -> Result<(), SessionError> {
        match &mut self.recorder {
            Some(recorder) => recorder.record(event).await,
            None => Ok(()),
        }
    }
}

/// `resume`：列出历史 session 并打开选择器。
pub(in crate::tui) async fn list_sessions(app: &mut App, session: &SessionBinding) {
    match session_store(session.recorder.as_ref()).await {
        Err(error) => app.warn(format!("{error:#}")),
        Ok(store) => match store.list_sessions().await {
            Err(error) => app.warn(format!("列出 session 失败：{error}")),
            Ok(sessions) if sessions.is_empty() => {
                app.chat_mut().push_system("没有历史 session。");
            }
            Ok(sessions) => {
                let rows = sessions
                    .iter()
                    .map(|summary| PickerRow {
                        id: summary.id.clone(),
                        text: crate::sessions::row_text(summary),
                        selectable: true,
                    })
                    .collect();
                app.open_resume_picker(rows);
            }
        },
    }
}

/// `new`：actor 邮箱串行清空上下文（fire-and-forget，紧随的 prompt 一定
/// 排在清空之后）；本地重置聊天区并新建 session。
///
/// 新 session 归属当前操作基准的 workspace（严格归属：用户在哪个
/// workspace 的上下文里操作，新 session 就属于哪个 workspace）；基准
/// 不变，工具无需切换。
pub(in crate::tui) async fn new_session(
    app: &mut App,
    session: &mut SessionBinding,
    handle: &AgentHandle,
) {
    // 命令仅在空闲时可提交，无需排队等待
    let _ = handle.clear_messages();
    app.start_new_conversation();
    // 先取基准快照以结束对 session 的借用（await 后要换绑 recorder）
    let base = session.base_dir();
    if let Some(recorder) = &mut session.recorder {
        match recorder.store().create_session(&base).await {
            Ok(new_id) => {
                // 换绑新 session：没有任何 entry，父指针重置（自动链最新）
                recorder.switch(new_id.clone(), None);
                app.set_session(new_id);
            }
            Err(error) => {
                app.warn(format!("创建新 session 失败，续写当前 session：{error}"));
            }
        }
    }
}

/// `tree`：列出当前 session 的会话树并打开选择器（预选中当前分支末端）。
pub(in crate::tui) async fn list_tree(app: &mut App, session: &SessionBinding) {
    let Some(recorder) = &session.recorder else {
        app.warn("当前对话未持久化，没有会话树可浏览");
        return;
    };
    match recorder.store().list_tree(recorder.session_id()).await {
        Err(error) => app.warn(format!("加载会话树失败：{error}")),
        Ok(entries) if entries.is_empty() => {
            app.chat_mut()
                .push_system("当前 session 还没有消息，发送一条后再来浏览会话树。");
        }
        Ok(entries) => {
            let rows = tree_rows(&entries, recorder.tip());
            // 预选中当前分支末端；末端不可选（工具结果，或已被折叠进摘要行）
            // 时退到首个可选行
            let selected = recorder
                .tip()
                .and_then(|tip| rows.iter().position(|row| row.id == tip))
                .filter(|&index| rows[index].selectable)
                .or_else(|| rows.iter().position(|row| row.selectable))
                .expect("空树已在上面挡掉");
            app.open_tree_picker(rows, selected);
        }
    }
}

/// `tree` 选择器确认：以所选条目为起点创建分支——重放该分支上下文、
/// 切换落库父指针；原分支 entries 不动，仍可在 `tree` 中回访。
pub(in crate::tui) async fn branch_to(
    app: &mut App,
    session: &mut SessionBinding,
    handle: &AgentHandle,
    entry_id: String,
) {
    let Some(recorder) = &session.recorder else {
        return; // ListTree 已挡住未持久化场景
    };
    if recorder.tip() == Some(entry_id.as_str()) {
        app.chat_mut()
            .push_system("所选条目就是当前分支末端，无需切换。");
        return;
    }
    // 提前克隆以结束对 session 的借用（await 后要切父指针）
    let store = recorder.store().clone();
    let session_id = recorder.session_id().to_string();
    match store.load_branch(&session_id, &entry_id).await {
        Err(error) => app.warn(format!("切换分支失败：{error}")),
        Ok(messages) => {
            // actor 邮箱 FIFO：紧随其后的 prompt 一定排在 Restore 之后
            if handle.restore_messages(messages.clone()).is_err() {
                app.warn("内部错误：agent 任务已退出，无法切换分支");
                return;
            }
            let count = messages.len();
            app.restore_branch(&messages);
            if let Some(recorder) = &mut session.recorder {
                recorder.set_tip(Some(entry_id));
            }
            app.chat_mut().push_system(format!(
                "已从所选条目创建分支（{count} 条消息），后续对话写入新分支；\
                 原分支保留，仍可在 /tree 中回访。"
            ));
        }
    }
}

/// 会话树条目 → 选择器行。
///
/// 缩进语义：**只在真实分叉处缩进**——用树形前缀（`├─`/`└─`/`│`）画出
/// 分支结构，线性链（含工具调用轮次）平铺。工具调用循环是父子链而非
/// 分支，若按祖先链长度缩进会把单线对话画成向右无限延伸的阶梯。
///
/// 不可选条目（含工具调用的 assistant 响应、工具结果）只是浏览上下文
/// 而非分支起点：连续的一段折叠为一行摘要（`↳ 工具调用 ×N（…）`），
/// 避免工具噪音淹没可选条目。折叠只取链上条目（子节点 ≤ 1），不会吞掉
/// 分叉点。
fn tree_rows(entries: &[TreeEntry], tip: Option<&str>) -> Vec<PickerRow> {
    let index: std::collections::HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (entry.id.as_str(), i))
        .collect();
    // 每个 entry 的子节点（按插入序）：判定分叉与折叠用
    let mut children: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, entry) in entries.iter().enumerate() {
        if let Some(parent) = entry.parent_id.as_deref() {
            children.entry(parent).or_default().push(i);
        }
    }
    // 树形前缀：沿父链收集「分叉边」（父节点有多个孩子的边）。entry 自己
    // 的分叉边渲染为 `├─ `/`└─ `；祖先的分叉边自外向内渲染为 `│  `/`   `
    // 层级。线性边不产生前缀。
    let prefix = |entry: &TreeEntry| {
        let mut ancestors: Vec<bool> = Vec::new(); // 祖先分叉边：该侧是否最末孩子
        let mut own: Option<bool> = None; // 自身边是否分叉
        let mut child = entry.id.as_str();
        let mut cursor = entry.parent_id.as_deref();
        let mut first = true;
        // 父指针在写入时校验存在，父链必然终止于根；缺失数据按根处理
        while let Some(parent) = cursor {
            let siblings = children.get(parent).map_or(&[][..], Vec::as_slice);
            if siblings.len() > 1 {
                let last = siblings.last().copied() == Some(index[child]);
                if first {
                    own = Some(last);
                } else {
                    ancestors.push(last);
                }
            }
            first = false;
            child = parent;
            cursor = index
                .get(parent)
                .and_then(|&i| entries[i].parent_id.as_deref());
        }
        let mut text = String::new();
        for &last in ancestors.iter().rev() {
            text.push_str(if last { "   " } else { "│  " });
        }
        if let Some(last) = own {
            text.push_str(if last { "└─ " } else { "├─ " });
        }
        text
    };
    // 可折叠：不可选且位于链上（子节点 ≤ 1）；有多子节点的条目是分叉点，
    // 必须保留原行以呈现树形结构
    let foldable = |entry: &TreeEntry| {
        !entry.is_branchable() && children.get(entry.id.as_str()).map_or(0, Vec::len) <= 1
    };
    let mut rows = Vec::with_capacity(entries.len());
    let mut i = 0;
    while i < entries.len() {
        if !foldable(&entries[i]) {
            rows.push(entry_row(&entries[i], tip, &prefix(&entries[i])));
            i += 1;
            continue;
        }
        let start = i;
        while i + 1 < entries.len() && foldable(&entries[i + 1]) {
            i += 1;
        }
        rows.push(fold_row(&entries[start..=i], tip, &prefix(&entries[start])));
        i += 1;
    }
    rows
}

/// 单条目的选择器行：树形前缀 + 角色/时间/预览，当前分支末端带标记。
fn entry_row(entry: &TreeEntry, tip: Option<&str>, prefix: &str) -> PickerRow {
    let role = match entry.role.as_str() {
        "user" => "用户",
        "assistant" => "助手",
        "tool_result" => "工具",
        _ => "压缩",
    };
    let current = if Some(entry.id.as_str()) == tip {
        "（当前）"
    } else {
        ""
    };
    PickerRow {
        id: entry.id.clone(),
        text: format!(
            "{prefix}{role} · {} · {}{current}",
            crate::sessions::format_time(Some(entry.timestamp)),
            entry.preview,
        ),
        selectable: entry.is_branchable(),
    }
}

/// 一段连续工具条目的折叠摘要行（不可选，仅浏览上下文）。
///
/// 工具名与失败数从工具结果 preview（`工具结果：{name}` / `工具失败：{name}`，
/// 见 nomic-session 的 `message_preview`）统计；preview 无法解析（如 payload
/// 损坏的占位文本）只计入总数。run 内含当前分支末端（运行中打开 `tree`）
/// 时带标记。
fn fold_row(run: &[TreeEntry], tip: Option<&str>, prefix: &str) -> PickerRow {
    let mut calls = 0_usize;
    let mut failures = 0_usize;
    let mut tools: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for entry in run.iter().filter(|entry| entry.role == "tool_result") {
        calls += 1;
        if let Some(name) = entry.preview.strip_prefix("工具结果：") {
            *tools.entry(name).or_default() += 1;
        } else if let Some(name) = entry.preview.strip_prefix("工具失败：") {
            failures += 1;
            *tools.entry(name).or_default() += 1;
        }
    }
    let mut parts: Vec<String> = tools
        .iter()
        .map(|(name, count)| format!("{name} ×{count}"))
        .collect();
    if failures > 0 {
        parts.push(format!("失败 ×{failures}"));
    }
    let stats = if parts.is_empty() {
        String::new()
    } else {
        format!("（{}）", parts.join(" · "))
    };
    // 无工具结果（如运行被打断）：退化为条目计数
    let summary = if calls > 0 {
        format!("工具调用 ×{calls}{stats}")
    } else {
        format!("工具调用 ×{}（无结果）", run.len())
    };
    let current = if run.iter().any(|entry| Some(entry.id.as_str()) == tip) {
        "（当前）"
    } else {
        ""
    };
    PickerRow {
        id: run[0].id.clone(),
        text: format!(
            "{prefix}↳ {summary} · {}{current}",
            crate::sessions::format_time(Some(run[0].timestamp)),
        ),
        selectable: false,
    }
}

/// 取可用 session store：优先复用 recorder 的；未持久化（启动时打开失败）
/// 时按需重开——`resume` 成功后该 store 会随新 recorder 一同被采用。
async fn session_store(recorder: Option<&SessionRecorder>) -> Result<SessionStore> {
    match recorder {
        Some(recorder) => Ok(recorder.store().clone()),
        None => SessionStore::open_default()
            .await
            .context("打开 session 库失败"),
    }
}

/// 恢复选中 session：加载历史 → 替换 agent 上下文与聊天区 → recorder
/// 换绑该 session（父指针为默认分支末端）。
pub(in crate::tui) async fn resume_session(
    app: &mut App,
    session: &mut SessionBinding,
    handle: &AgentHandle,
    id: String,
) {
    let loaded = async {
        let store = session_store(session.recorder.as_ref()).await?;
        let messages = store
            .load_messages(&id)
            .await
            .with_context(|| "加载 session 历史失败".to_string())?;
        let tip = store
            .latest_entry_id(&id)
            .await
            .context("读取分支末端失败")?;
        let workspace = store
            .session_workspace_path(&id)
            .await
            .context("读取 session 的 workspace 失败")?;
        Ok::<_, anyhow::Error>((store, messages, tip, workspace))
    }
    .await;
    match loaded {
        Err(error) => app.warn(format!("恢复 session 失败：{error:#}")),
        Ok((store, messages, tip, workspace)) => {
            // actor 邮箱 FIFO：紧随其后的 prompt 一定排在 Restore 之后，
            // 不会出现「新 prompt 跑在旧上下文」的交错
            let _ = handle.restore_messages(messages.clone());
            app.restore_conversation(&messages, id.clone());
            // workspace 严格归属：操作基准（工具相对路径、mention 展开）切到
            // 所恢复 session 的 workspace；句柄与工具共享，下一次执行即生效
            let previous = session.base_dir();
            session.base.set(workspace.clone());
            match &mut session.recorder {
                Some(recorder) => recorder.switch(id.clone(), tip),
                None => {
                    session.recorder = Some(SessionRecorder::with_tip(store, id.clone(), tip));
                }
            }
            let label = nomic_session::session_title(&messages)
                .map_or_else(String::new, |title| format!("「{title}」"));
            app.chat_mut().push_system(format!(
                "已恢复 session {label}（{} 条消息），后续对话续写该 session。",
                messages.len()
            ));
            if workspace != previous {
                app.chat_mut().push_system(format!(
                    "操作基准已切换到该 session 的 workspace：{}",
                    workspace.display()
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{App, BaseDir, SessionBinding, SessionRecorder, SessionStore, tree_rows};
    use nomic_ai::{Message, UserMessage, UserMessageContent};
    use nomic_session::TreeEntry;

    /// `resume` 跨 workspace：recorder 换绑目标 session，操作基准（工具与
    /// mention 共用的句柄）切到该 session 的 workspace，并给出可见提示。
    #[tokio::test]
    async fn resume_switches_base_dir_to_session_workspace() {
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let store = SessionStore::in_memory().await.expect("store");
        let session_a = store.create_session(dir_a.path()).await.expect("create a");
        let session_b = store.create_session(dir_b.path()).await.expect("create b");
        store
            .append_message(
                &session_b,
                None,
                &Message::User(UserMessage {
                    content: UserMessageContent::Text("hello from b".to_string()),
                    timestamp: 1000,
                }),
            )
            .await
            .expect("append b");

        let base = BaseDir::new(Some(dir_a.path().to_path_buf()));
        let mut binding =
            SessionBinding::new(Some(SessionRecorder::new(store, session_a)), base.clone());
        let mut app = App::new("test-model".to_string(), None, 200_000);
        let handle = crate::tui::driver::dummy_handle();

        super::resume_session(&mut app, &mut binding, &handle, session_b.clone()).await;

        // Restore 已经 actor 邮箱 FIFO 生效（紧随的查询一定看到）
        assert_eq!(
            handle.messages().await.expect("查询应成功").len(),
            1,
            "恢复的上下文应替换进 agent"
        );
        assert_eq!(
            binding.recorder.as_ref().expect("recorder").session_id(),
            session_b,
            "recorder 应换绑到恢复的 session"
        );
        assert_eq!(
            binding.base_dir(),
            std::fs::canonicalize(dir_b.path()).expect("canonicalize"),
            "操作基准应切到所恢复 session 的 workspace"
        );
    }

    /// `new` 新建 session 归属当前操作基准的 workspace（基准不变）。
    #[tokio::test]
    async fn new_session_binds_current_base_workspace() {
        let dir_a = tempfile::tempdir().expect("tempdir a");
        let store = SessionStore::in_memory().await.expect("store");
        let session_a = store.create_session(dir_a.path()).await.expect("create a");

        let base = BaseDir::new(Some(dir_a.path().to_path_buf()));
        let mut binding =
            SessionBinding::new(Some(SessionRecorder::new(store, session_a)), base.clone());
        let mut app = App::new("test-model".to_string(), None, 200_000);
        let handle = crate::tui::driver::dummy_handle();

        super::new_session(&mut app, &mut binding, &handle).await;

        // Clear 已经 actor 邮箱 FIFO 生效（紧随的查询一定看到）
        assert!(
            handle.messages().await.expect("查询应成功").is_empty(),
            "上下文应已清空"
        );
        let new_id = binding.recorder.as_ref().expect("recorder").session_id();
        let workspace = binding
            .recorder
            .as_ref()
            .expect("recorder")
            .store()
            .session_workspace_path(new_id)
            .await
            .expect("workspace");
        assert_eq!(
            workspace,
            std::fs::canonicalize(dir_a.path()).expect("canonicalize"),
            "新 session 应归属当前基准的 workspace"
        );
    }

    /// 会话树选择器行：线性链（含工具调用轮次）平铺不缩进，连续工具条目
    /// 折叠为一行摘要（不可选），当前分支末端带标记。
    #[test]
    fn tree_rows_flatten_linear_chain_and_fold_tools() {
        let entry = |id: &str, parent: Option<&str>, role: &str, tool_calls: bool| TreeEntry {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            role: role.to_string(),
            timestamp: 1_785_000_000_000,
            preview: format!("preview of {id}"),
            has_tool_calls: tool_calls,
        };
        let tool_result = |id: &str, parent: &str, name: &str, failed: bool| {
            let mut entry = entry(id, Some(parent), "tool_result", false);
            entry.preview = if failed {
                format!("工具失败：{name}")
            } else {
                format!("工具结果：{name}")
            };
            entry
        };
        let entries = vec![
            entry("root", None, "user", false),
            entry("a1", Some("root"), "assistant", true),
            tool_result("t1", "a1", "bash", false),
            tool_result("t2", "t1", "bash", true),
            entry("a2", Some("t2"), "assistant", false),
        ];

        let rows = tree_rows(&entries, Some("t2"));
        assert_eq!(rows.len(), 3, "工具条目折叠为一行：{rows:?}");
        assert!(rows[0].text.starts_with("用户 · "), "{}", rows[0].text);
        assert!(
            rows[1]
                .text
                .starts_with("↳ 工具调用 ×2（bash ×2 · 失败 ×1）"),
            "{}",
            rows[1].text
        );
        assert!(rows[1].text.ends_with("（当前）"), "{}", rows[1].text);
        assert!(
            rows[2].text.starts_with("助手 · "),
            "线性链不缩进：{}",
            rows[2].text
        );

        assert!(rows[0].selectable);
        assert!(!rows[1].selectable, "折叠摘要行不可选");
        assert!(rows[2].selectable);
    }

    /// 会话树选择器行：真实分叉用树形前缀（`├─`/`└─`/`│`）画分支结构，
    /// 分叉下的线性后代继承层级前缀。
    #[test]
    fn tree_rows_draw_branch_prefixes_at_forks() {
        let entry = |id: &str, parent: Option<&str>| TreeEntry {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            role: "user".to_string(),
            timestamp: 1_785_000_000_000,
            preview: format!("preview of {id}"),
            has_tool_calls: false,
        };
        let entries = vec![
            entry("root", None),
            entry("b1", Some("root")),
            entry("c1", Some("b1")),
            entry("b2", Some("root")),
            entry("c2", Some("b2")),
        ];

        let rows = tree_rows(&entries, Some("c2"));
        assert_eq!(rows.len(), 5);
        assert!(rows[0].text.starts_with("用户 · "), "{}", rows[0].text);
        assert!(rows[1].text.starts_with("├─ "), "{}", rows[1].text);
        assert!(
            rows[2].text.starts_with("│  "),
            "非最末分支的后代画竖线：{}",
            rows[2].text
        );
        assert!(rows[3].text.starts_with("└─ "), "{}", rows[3].text);
        assert!(
            rows[4].text.starts_with("   "),
            "最末分支的后代留白：{}",
            rows[4].text
        );
        assert!(rows[4].text.ends_with("（当前）"), "{}", rows[4].text);
        assert!(rows.iter().all(|row| row.selectable));
    }

    /// 折叠不吞分叉点：不可选条目若有多个子节点（历史数据防御），保留原行。
    #[test]
    fn tree_rows_keep_unselectable_fork_point() {
        let entry = |id: &str, parent: Option<&str>, role: &str, tool_calls: bool| TreeEntry {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            role: role.to_string(),
            timestamp: 1_785_000_000_000,
            preview: format!("preview of {id}"),
            has_tool_calls: tool_calls,
        };
        let entries = vec![
            entry("root", None, "user", false),
            entry("a1", Some("root"), "assistant", true),
            entry("b1", Some("a1"), "user", false),
            entry("b2", Some("a1"), "user", false),
        ];

        let rows = tree_rows(&entries, None);
        assert_eq!(rows.len(), 4, "分叉点不折叠：{rows:?}");
        assert!(!rows[1].selectable, "含工具调用的 assistant 条目不可选");
        assert!(rows[2].text.starts_with("├─ "), "{}", rows[2].text);
        assert!(rows[3].text.starts_with("└─ "), "{}", rows[3].text);
    }
}
