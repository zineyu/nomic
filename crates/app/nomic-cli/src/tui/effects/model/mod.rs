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
    let Some(setting) = reasoning_setting(word) else {
        // 理论不可达（选择器行 id 出自 REASONING_LEVELS 词表）
        app.warn(format!("未知思考级别 {word:?}"));
        return;
    };
    match switcher.confirm_level(setting.level(), job_tx) {
        Confirm::Done { notice, persist } => {
            if let Some(spec) = persist {
                persist_model_selection(session, spec);
                update_badge(app, switcher);
            }
            persist_reasoning(session, setting.level());
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
    let value = match level {
        Some(ThinkingLevel::Minimal) => "minimal",
        Some(ThinkingLevel::Low) => "low",
        Some(ThinkingLevel::Medium) => "medium",
        Some(ThinkingLevel::High) => "high",
        _ => "off",
    };
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

/// 思考级别选择器确认时的解析结果：关闭（`off`）或具体级别。
///
/// 独立于 `Option<ThinkingLevel>`：让「行 id 非法」（None，拒绝）与
/// 「off 关闭」（合法设置）在类型层面可区分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReasoningSetting {
    /// 关闭思考
    Off,
    /// 具体思考级别
    Level(ThinkingLevel),
}

impl ReasoningSetting {
    /// 转为请求参数（`Off` → `None` 关闭）。
    const fn level(self) -> Option<ThinkingLevel> {
        match self {
            Self::Off => None,
            Self::Level(level) => Some(level),
        }
    }
}

/// 思考级别词表：选择器行 id 与展示说明共用同一来源。
const REASONING_LEVELS: [(&str, ReasoningSetting); 5] = [
    ("off", ReasoningSetting::Off),
    ("minimal", ReasoningSetting::Level(ThinkingLevel::Minimal)),
    ("low", ReasoningSetting::Level(ThinkingLevel::Low)),
    ("medium", ReasoningSetting::Level(ThinkingLevel::Medium)),
    ("high", ReasoningSetting::Level(ThinkingLevel::High)),
];

/// 级别词 → 设置；未知词返回 `None`（调用方告警）。
fn reasoning_setting(word: &str) -> Option<ReasoningSetting> {
    REASONING_LEVELS
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(_, setting)| *setting)
}

/// 当前级别 → 词表中的级别词（提示文本用；词表外取值回退 `off`）。
pub(super) fn reasoning_label(level: Option<ThinkingLevel>) -> &'static str {
    REASONING_LEVELS
        .iter()
        .find(|(_, setting)| setting.level() == level)
        .map_or("off", |(name, _)| *name)
}

/// 思考级别选择器（模型切换流程第二步）：列出级别并打开选择器
///（预选中当前级别）。
fn open_reasoning_picker(app: &mut App, current: Option<ThinkingLevel>) {
    let rows = REASONING_LEVELS
        .iter()
        .map(|(name, setting)| PickerRow {
            id: (*name).to_string(),
            text: reasoning_row_text(name, *setting, current),
            selectable: true,
        })
        .collect();
    let selected = REASONING_LEVELS
        .iter()
        .position(|(_, setting)| setting.level() == current)
        .unwrap_or(0);
    app.open_reasoning_picker(rows, selected);
}

/// 思考级别选择器行文本：`级别 — 说明`，当前级别带标记。
fn reasoning_row_text(
    name: &str,
    setting: ReasoningSetting,
    current: Option<ThinkingLevel>,
) -> String {
    let description = match setting {
        ReasoningSetting::Off => "不开启思考",
        ReasoningSetting::Level(ThinkingLevel::Minimal) => "最小推理预算",
        ReasoningSetting::Level(ThinkingLevel::Low) => "低推理预算",
        ReasoningSetting::Level(ThinkingLevel::Medium) => "中等推理预算",
        ReasoningSetting::Level(ThinkingLevel::High) => "高推理预算",
        // xhigh/max 不在 TUI 词表内（配置文件与 CLI 同样不开放）
        ReasoningSetting::Level(ThinkingLevel::Xhigh | ThinkingLevel::Max) => "推理预算",
    };
    let mut text = format!("{name} — {description}");
    if setting.level() == current {
        text.push_str("（当前）");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{
        ModelChoice, ModelSelection, ReasoningSetting, model_row_text, reasoning_label,
        reasoning_row_text, reasoning_setting,
    };
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

    /// 思考级别词表：off 映射为关闭，词表内级别往返一致，未知词拒绝。
    #[test]
    fn reasoning_setting_roundtrip_and_rejects_unknown() {
        assert_eq!(reasoning_setting("off"), Some(ReasoningSetting::Off));
        assert_eq!(
            reasoning_setting("minimal"),
            Some(ReasoningSetting::Level(ThinkingLevel::Minimal))
        );
        assert_eq!(
            reasoning_setting("high"),
            Some(ReasoningSetting::Level(ThinkingLevel::High))
        );
        assert_eq!(
            reasoning_setting("off").map(ReasoningSetting::level),
            Some(None)
        );
        assert_eq!(reasoning_setting("extreme"), None);
        // 词表内取值与 label 往返一致；词表外取值（xhigh/max）回退 off
        for (name, level) in [
            ("off", None),
            ("low", Some(ThinkingLevel::Low)),
            ("medium", Some(ThinkingLevel::Medium)),
            ("high", Some(ThinkingLevel::High)),
        ] {
            assert_eq!(reasoning_label(level), name);
        }
        assert_eq!(reasoning_label(Some(ThinkingLevel::Xhigh)), "off");
    }

    /// 思考级别选择器行：级别 + 说明，当前级别带标记。
    #[test]
    fn reasoning_row_text_marks_current() {
        assert_eq!(
            reasoning_row_text(
                "low",
                ReasoningSetting::Level(ThinkingLevel::Low),
                Some(ThinkingLevel::Low)
            ),
            "low — 低推理预算（当前）"
        );
        assert_eq!(
            reasoning_row_text("off", ReasoningSetting::Off, Some(ThinkingLevel::Low)),
            "off — 不开启思考"
        );
    }
}
