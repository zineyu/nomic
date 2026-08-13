//! 会话管理：`/resume` 恢复、`/tree` 浏览与分支、`/new` 新建，以及
//! MessageEnd / CompactionEnd 定稿点落库（父指针推进）。

use anyhow::{Context as _, Result};
use nomic_ai::Message;
use nomic_session::{CompactionRecord, SessionStore, TreeEntry};

use crate::tui::app::{App, PickerRow};
use crate::tui::driver::{Driver, DriverJob};

/// `/resume`：列出历史 session 并打开选择器。
pub(in crate::tui) async fn list_sessions(app: &mut App, driver: &Driver) {
    match session_store(driver.session.as_ref()).await {
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

/// `/new`：driver 串行清空上下文；本地重置聊天区并新建 session。
pub(in crate::tui) async fn new_session(app: &mut App, driver: &mut Driver) {
    // driver 串行处理任务；slash 命令仅在空闲时可提交，无需排队等待
    let _ = driver.job_tx.send(DriverJob::Clear);
    app.start_new_conversation();
    // 新 session 没有任何 entry：落库父指针重置（自动链最新）
    driver.tip = None;
    if let Some((store, id)) = &mut driver.session {
        match store.create_session(&driver.cwd).await {
            Ok(new_id) => {
                id.clone_from(&new_id);
                app.set_session(new_id);
            }
            Err(error) => {
                app.warn(format!("创建新 session 失败，续写当前 session：{error}"));
            }
        }
    }
}

/// `/tree`：列出当前 session 的会话树并打开选择器（预选中当前分支末端）。
pub(in crate::tui) async fn list_tree(app: &mut App, driver: &Driver) {
    let Some((store, session_id)) = &driver.session else {
        app.warn("当前对话未持久化，没有会话树可浏览");
        return;
    };
    match store.list_tree(session_id).await {
        Err(error) => app.warn(format!("加载会话树失败：{error}")),
        Ok(entries) if entries.is_empty() => {
            app.chat_mut()
                .push_system("当前 session 还没有消息，发送一条后再来浏览会话树。");
        }
        Ok(entries) => {
            let rows = tree_rows(&entries, driver.tip.as_deref());
            // 预选中当前分支末端；末端不可选（工具结果，或已被折叠进摘要行）
            // 时退到首个可选行
            let selected = driver
                .tip
                .as_deref()
                .and_then(|tip| rows.iter().position(|row| row.id == tip))
                .filter(|&index| rows[index].selectable)
                .or_else(|| rows.iter().position(|row| row.selectable))
                .expect("空树已在上面挡掉");
            app.open_tree_picker(rows, selected);
        }
    }
}

/// `/tree` 选择器确认：以所选条目为起点创建分支——重放该分支上下文、
/// 切换落库父指针；原分支 entries 不动，仍可在 `/tree` 中回访。
pub(in crate::tui) async fn branch_to(app: &mut App, driver: &mut Driver, entry_id: String) {
    let Some((store, session_id)) = &driver.session else {
        return; // ListTree 已挡住未持久化场景
    };
    if driver.tip.as_deref() == Some(entry_id.as_str()) {
        app.chat_mut()
            .push_system("所选条目就是当前分支末端，无需切换。");
        return;
    }
    match store.load_branch(session_id, &entry_id).await {
        Err(error) => app.warn(format!("切换分支失败：{error}")),
        Ok(messages) => {
            // driver 串行处理任务：紧随其后的 prompt 一定排在 Restore 之后
            if driver
                .job_tx
                .send(DriverJob::Restore(messages.clone()))
                .is_err()
            {
                app.warn("内部错误：agent 任务已退出，无法切换分支");
                return;
            }
            let count = messages.len();
            app.restore_branch(&messages);
            driver.tip = Some(entry_id);
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
/// 损坏的占位文本）只计入总数。run 内含当前分支末端（运行中打开 `/tree`）
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

/// 取可用 session store：优先复用当前 session 的；未持久化（启动时打开失败）
/// 时按需重开——`/resume` 成功后该 store 会随恢复的 session 一同被采用。
async fn session_store(session: Option<&(SessionStore, String)>) -> Result<SessionStore> {
    match session {
        Some((store, _)) => Ok(store.clone()),
        None => SessionStore::open_default()
            .await
            .context("打开 session 库失败"),
    }
}

/// 恢复选中 session：加载历史 → 替换 agent 上下文与聊天区 → 切换落库目标
/// 与落库父指针（默认分支末端）。
pub(in crate::tui) async fn resume_session(app: &mut App, driver: &mut Driver, id: String) {
    let loaded = async {
        let store = session_store(driver.session.as_ref()).await?;
        let messages = store
            .load_messages(&id)
            .await
            .with_context(|| "加载 session 历史失败".to_string())?;
        let tip = store
            .latest_entry_id(&id)
            .await
            .context("读取分支末端失败")?;
        Ok::<_, anyhow::Error>((store, messages, tip))
    }
    .await;
    match loaded {
        Err(error) => app.warn(format!("恢复 session 失败：{error:#}")),
        Ok((store, messages, tip)) => {
            // driver 串行处理任务：紧随其后的 prompt 一定排在 Restore 之后，
            // 不会出现「新 prompt 跑在旧上下文」的交错
            let _ = driver.job_tx.send(DriverJob::Restore(messages.clone()));
            app.restore_conversation(&messages, id.clone());
            driver.tip = tip;
            match &mut driver.session {
                Some((_, current)) => current.clone_from(&id),
                None => driver.session = Some((store, id.clone())),
            }
            let label = nomic_session::session_title(&messages)
                .map_or_else(String::new, |title| format!("「{title}」"));
            app.chat_mut().push_system(format!(
                "已恢复 session {label}（{} 条消息），后续对话续写该 session。",
                messages.len()
            ));
        }
    }
}

/// `MessageEnd` 定稿点落库：以当前分支末端为父 entry，成功后推进父指针；
/// 失败仅提示不中断（store 非权威源）。
pub(in crate::tui) async fn persist(driver: &mut Driver, message: &Message, app: &mut App) {
    let Some((store, session_id)) = &driver.session else {
        return;
    };
    match store
        .append_message(session_id, driver.tip.as_deref(), message)
        .await
    {
        Ok(entry_id) => driver.tip = Some(entry_id),
        Err(error) => app.warn(format!("session 落库失败：{error}")),
    }
}

/// `CompactionEnd` 落库压缩条目（父指针语义与 [`persist`] 一致）。
pub(in crate::tui) async fn persist_compaction(
    driver: &mut Driver,
    summary: &str,
    tokens_before: u64,
    kept_count: usize,
    app: &mut App,
) {
    let Some((store, session_id)) = &driver.session else {
        return;
    };
    let record = CompactionRecord {
        summary: summary.to_string(),
        kept_count: kept_count as u64,
        tokens_before,
    };
    match store
        .append_compaction(session_id, driver.tip.as_deref(), &record)
        .await
    {
        Ok(entry_id) => driver.tip = Some(entry_id),
        Err(error) => app.warn(format!("compaction 落库失败：{error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::tree_rows;
    use nomic_session::TreeEntry;

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
