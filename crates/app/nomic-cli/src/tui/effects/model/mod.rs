//! 模型 + 思考级别两步流（`models`）的选择器 UI 接线：先选模型，
//! 推理模型再选思考级别（第二步确认时一并应用切换，Esc 放弃）。
//!
//! 流程状态与不变量（待切换模型暂存、切换/级别幂等、job 顺序、跨
//! provider 新连接）收在 [`switch::ModelSwitcher`] 状态机；本模块只做
//! 选择器行构建/预选、app 状态栏更新与选择落库接线。

use nomic_ai::ThinkingLevel;
use tokio::sync::mpsc;

use super::session::SessionBinding;
use crate::model::{self, ModelChoice, ModelSelection};
use crate::tui::app::{App, PickerRow};
use crate::tui::driver::DriverJob;
use crate::tui::widgets;

mod switch;

pub(in crate::tui) use switch::ModelSwitcher;
use switch::{Confirm, Select};

/// `models`：跨 provider 列出候选模型并打开选择器（预选中当前模型）。
pub(in crate::tui) fn list_models(app: &mut App, switcher: &ModelSwitcher) {
    let (current, choices) = switcher.candidates();
    if choices.is_empty() {
        // 理论不可达（候选至少含内置 provider 的默认模型），防御配置在运行期失效
        app.warn("没有可用的模型候选");
        return;
    }
    let selected = choices
        .iter()
        .position(|choice| is_current(choice, &current))
        .unwrap_or(0);
    let rows = choices
        .iter()
        .map(|choice| PickerRow {
            id: choice.spec(),
            text: model_row_text(choice, &current),
            selectable: true,
        })
        .collect();
    app.open_model_picker(rows, selected);
}

/// `models:<p>/<id>` 或模型选择器确认（流程第一步）：转发给状态机，
/// 按流转结果打开级别选择器 / 更新徽标并落库 / 提示。
pub(in crate::tui) fn select_model(
    app: &mut App,
    switcher: &mut ModelSwitcher,
    job_tx: &mpsc::UnboundedSender<DriverJob>,
    session: &SessionBinding,
    id: &str,
) {
    match switcher.select(id, job_tx) {
        Select::AwaitLevel => open_reasoning_picker(app, switcher.reasoning()),
        Select::Switched { notice, persist } => {
            persist_model_selection(session, persist);
            update_badge(app, switcher);
            app.chat_mut().push_system(notice);
        }
        Select::AlreadyCurrent { notice } => app.chat_mut().push_system(notice),
        Select::Failed(warn) => app.warn(warn),
    }
}

/// 思考级别选择器确认（流程第二步）：转发给状态机应用待切换模型并
/// 设置级别；应用了切换时更新徽标并落库。
pub(in crate::tui) fn set_reasoning(
    app: &mut App,
    switcher: &mut ModelSwitcher,
    job_tx: &mpsc::UnboundedSender<DriverJob>,
    session: &SessionBinding,
    word: &str,
) {
    // 理论不可达（选择器行 id 出自 REASONING_LEVELS 词表）
    let level = match ThinkingLevel::parse_setting(word) {
        Ok(level) => level,
        Err(error) => {
            app.warn(error.to_string());
            return;
        }
    };
    match switcher.confirm_level(level, job_tx) {
        Confirm::Done { notice, persist } => {
            if let Some(spec) = persist {
                persist_model_selection(session, spec);
                update_badge(app, switcher);
            }
            persist_reasoning(session, level);
            app.chat_mut().push_system(notice);
        }
        Confirm::Failed(warn) => app.warn(warn),
    }
}

/// 模型切换流程第二步被取消（Esc）：放弃待切换模型，模型与级别均不变。
pub(in crate::tui) fn cancel_model_switch(app: &mut App, switcher: &mut ModelSwitcher) {
    if switcher.cancel() {
        app.chat_mut().push_system("已取消模型切换。".to_string());
    }
}

/// 切换成功后同步状态栏徽标（模型名与上下文窗口）。
fn update_badge(app: &mut App, switcher: &ModelSwitcher) {
    let current = switcher.current();
    app.set_model(current.name.clone(), current.context_window);
}

/// 模型选择落库（config 表 append-only，最新行即下次启动的首选）。
///
/// 库不可用（启动已告警）时跳过；写失败只记日志不打断切换——
/// 下次启动的回退链只是少了这一条。
fn persist_model_selection(session: &SessionBinding, spec: String) {
    let Some(store) = session.store() else {
        return;
    };
    tokio::spawn(async move {
        if let Err(error) = store
            .set_config(model::CONFIG_KEY_MODEL, &serde_json::Value::String(spec))
            .await
        {
            tracing::warn!(error = %error, "模型选择落库失败");
        }
    });
}

/// 思考级别落库（config 表 append-only，最新行即下次启动的恢复源）。
fn persist_reasoning(session: &SessionBinding, level: Option<ThinkingLevel>) {
    let Some(store) = session.store() else {
        return;
    };
    let value = level.map_or("off", ThinkingLevel::as_str);
    tokio::spawn(async move {
        if let Err(error) = store
            .set_config(
                model::CONFIG_KEY_REASONING,
                &serde_json::Value::String(value.to_string()),
            )
            .await
        {
            tracing::warn!(error = %error, "思考级别落库失败");
        }
    });
}

/// 候选行是否为当前模型（provider 与模型 id 均相同）。
fn is_current(choice: &ModelChoice, current: &ModelSelection) -> bool {
    (choice.provider.as_str(), choice.id.as_str())
        == (current.provider.as_str(), current.model.as_str())
}

/// 选择器行文本：`<provider>/<模型id> — 展示名 · ctx 200k · 支持思考`，
/// 当前模型带标记；窗口未知省略 ctx。
fn model_row_text(choice: &ModelChoice, current: &ModelSelection) -> String {
    use std::fmt::Write as _;
    let mut text = format!("{} — {}", choice.spec(), choice.name);
    if choice.context_window > 0 {
        let _ = write!(
            text,
            " · ctx {}",
            widgets::format_tokens(choice.context_window)
        );
    }
    if choice.reasoning {
        text.push_str(" · 支持思考");
    }
    if is_current(choice, current) {
        text.push_str("（当前）");
    }
    text
}

/// 思考级别词表：选择器行顺序与展示说明共用同一来源，行 id 取自
/// [`ThinkingLevel::as_str`] / `"off"`（解析侧统一走
/// [`ThinkingLevel::parse_setting`]，行 id 非法与 `off` 关闭由 `Result` 区分）。
/// xhigh/max 不在 TUI 词表内（配置文件与 CLI 同样不开放）。
const REASONING_LEVELS: [Option<ThinkingLevel>; 5] = [
    None,
    Some(ThinkingLevel::Minimal),
    Some(ThinkingLevel::Low),
    Some(ThinkingLevel::Medium),
    Some(ThinkingLevel::High),
];

/// 当前级别 → 级别词（提示文本用；`None` 即 `off`）。
pub(super) fn reasoning_label(level: Option<ThinkingLevel>) -> &'static str {
    level.map_or("off", ThinkingLevel::as_str)
}

/// 思考级别选择器（模型切换流程第二步）：列出级别并打开选择器
///（预选中当前级别）。
fn open_reasoning_picker(app: &mut App, current: Option<ThinkingLevel>) {
    let rows = REASONING_LEVELS
        .iter()
        .map(|level| PickerRow {
            id: reasoning_label(*level).to_string(),
            text: reasoning_row_text(*level, current),
            selectable: true,
        })
        .collect();
    let selected = REASONING_LEVELS
        .iter()
        .position(|level| *level == current)
        .unwrap_or(0);
    app.open_reasoning_picker(rows, selected);
}

/// 思考级别选择器行文本：`级别 — 说明`，当前级别带标记。
fn reasoning_row_text(level: Option<ThinkingLevel>, current: Option<ThinkingLevel>) -> String {
    let description = match level {
        None => "不开启思考",
        Some(ThinkingLevel::Minimal) => "最小推理预算",
        Some(ThinkingLevel::Low) => "低推理预算",
        Some(ThinkingLevel::Medium) => "中等推理预算",
        Some(ThinkingLevel::High) => "高推理预算",
        // xhigh/max 不在 TUI 词表内（配置文件与 CLI 同样不开放）
        Some(ThinkingLevel::Xhigh | ThinkingLevel::Max) => "推理预算",
    };
    let mut text = format!("{} — {description}", reasoning_label(level));
    if level == current {
        text.push_str("（当前）");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{ModelChoice, ModelSelection, model_row_text, reasoning_label, reasoning_row_text};
    use nomic_ai::ThinkingLevel;

    /// `models` 选择器行：id + 展示名 + 窗口，推理模型带标注，当前模型带标记，
    /// 窗口未知省略 ctx。
    #[test]
    fn model_row_text_formats_window_and_marks_current() {
        let choice = ModelChoice {
            provider: "openai".to_string(),
            id: "gpt-5.2".to_string(),
            name: "GPT-5.2".to_string(),
            context_window: 400_000,
            reasoning: true,
        };
        let current = ModelSelection::parse("openai/gpt-5.2", None).unwrap();
        assert_eq!(
            model_row_text(&choice, &current),
            "openai/gpt-5.2 — GPT-5.2 · ctx 400k · 支持思考（当前）"
        );
        let other = ModelSelection::parse("openai/other", None).unwrap();
        assert_eq!(
            model_row_text(&choice, &other),
            "openai/gpt-5.2 — GPT-5.2 · ctx 400k · 支持思考"
        );
        // 同名模型 id 但 provider 不同：不是当前模型
        let other_provider = ModelSelection::parse("deepseek/gpt-5.2", None).unwrap();
        assert!(!model_row_text(&choice, &other_provider).contains("（当前）"));
        let no_thinking = ModelChoice {
            reasoning: false,
            ..choice
        };
        assert_eq!(
            model_row_text(&no_thinking, &other),
            "openai/gpt-5.2 — GPT-5.2 · ctx 400k"
        );
        let unknown = ModelChoice {
            provider: "openai".to_string(),
            id: "m".to_string(),
            name: "m".to_string(),
            context_window: 0,
            reasoning: false,
        };
        assert_eq!(model_row_text(&unknown, &other), "openai/m — m");
    }

    /// 思考级别词表：off 映射为关闭，词表内级别 label 往返一致。
    #[test]
    fn reasoning_label_roundtrips_with_parse_setting() {
        for (name, level) in [
            ("off", None),
            ("minimal", Some(ThinkingLevel::Minimal)),
            ("low", Some(ThinkingLevel::Low)),
            ("medium", Some(ThinkingLevel::Medium)),
            ("high", Some(ThinkingLevel::High)),
        ] {
            assert_eq!(reasoning_label(level), name);
            assert_eq!(ThinkingLevel::parse_setting(name), Ok(level));
        }
        assert!(ThinkingLevel::parse_setting("extreme").is_err());
        // 词表外取值（xhigh/max）label 如实显示级别词
        assert_eq!(reasoning_label(Some(ThinkingLevel::Xhigh)), "xhigh");
    }

    /// 思考级别选择器行：级别 + 说明，当前级别带标记。
    #[test]
    fn reasoning_row_text_marks_current() {
        assert_eq!(
            reasoning_row_text(Some(ThinkingLevel::Low), Some(ThinkingLevel::Low)),
            "low — 低推理预算（当前）"
        );
        assert_eq!(
            reasoning_row_text(None, Some(ThinkingLevel::Low)),
            "off — 不开启思考"
        );
    }
}
