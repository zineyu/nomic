//! 模型 + 思考级别两步流：`/models` 先选模型，推理模型再选思考级别
//! （第二步确认时一并应用切换，Esc 放弃）；选择随 sqlite 配置表落库。

use nomic_ai::{Model, ThinkingLevel};

use crate::model::{self, ModelChoice, ModelSelection};
use crate::tui::app::{App, PickerRow};
use crate::tui::widgets;
use crate::tui::{Driver, DriverJob, ModelSwitch, ProviderSwitch};

/// `/models`：跨 provider 列出候选模型并打开选择器（预选中当前模型）。
pub(in crate::tui) fn list_models(app: &mut App, driver: &Driver) {
    let current = current_selection(&driver.model);
    let choices = driver.models.candidates(&current);
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

/// 候选行是否为当前模型（provider 与模型 id 均相同）。
fn is_current(choice: &ModelChoice, current: &ModelSelection) -> bool {
    (choice.provider.as_str(), choice.id.as_str())
        == (current.provider.as_str(), current.model.as_str())
}

/// 当前模型的选择项（`<provider>/<模型id>`）。
fn current_selection(model: &Model) -> ModelSelection {
    ModelSelection {
        provider: model.provider.clone(),
        model: model.id.clone(),
    }
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
fn reasoning_label(level: Option<ThinkingLevel>) -> &'static str {
    REASONING_LEVELS
        .iter()
        .find(|(_, setting)| setting.level() == level)
        .map_or("off", |(name, _)| *name)
}

/// 思考级别选择器（模型切换流程第二步）：列出级别并打开选择器
///（预选中当前级别）。
fn open_reasoning_picker(app: &mut App, driver: &Driver) {
    let current = driver.reasoning;
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

/// 思考级别选择器确认（模型切换流程第二步）：先应用待切换模型，
/// 再设置思考级别；两者均未变化时提示。
///
/// 级别是请求参数，选择器只在目标模型支持推理时出现，因此设置必然
/// 随请求生效（重选当前模型进入时当前模型即推理模型）。driver 串行
/// 处理任务：级别设置一定排在模型切换之后。
pub(in crate::tui) fn set_reasoning(app: &mut App, driver: &mut Driver, word: &str) {
    let Some(setting) = reasoning_setting(word) else {
        // 理论不可达（选择器行 id 出自 REASONING_LEVELS 词表）
        app.warn(format!("未知思考级别 {word:?}"));
        return;
    };
    let level = setting.level();
    let switched = match driver.pending_model.take() {
        Some(model) => apply_model_switch(app, driver, model),
        None => false,
    };
    let level_changed = level != driver.reasoning;
    if level_changed {
        if driver.job_tx.send(DriverJob::SetReasoning(level)).is_err() {
            app.warn("内部错误：agent 任务已退出，无法设置思考级别");
            return;
        }
        driver.reasoning = level;
    }
    let mut parts: Vec<String> = Vec::new();
    if switched {
        parts.push(switched_part(&driver.model));
    }
    if level_changed {
        parts.push(format!("思考级别设为 {}", reasoning_label(level)));
    }
    let text = if parts.is_empty() {
        format!(
            "模型与思考级别均未变化（{}，级别 {}）。",
            driver.model.name,
            reasoning_label(driver.reasoning)
        )
    } else if switched {
        format!("{}，对话上下文保留。", parts.join("，"))
    } else {
        format!("{}。", parts.join("，"))
    };
    app.chat_mut().push_system(text);
}

/// 模型切换流程第二步被取消（Esc）：放弃待切换模型，模型与级别均不变。
pub(in crate::tui) fn cancel_model_switch(app: &mut App, driver: &mut Driver) {
    if driver.pending_model.take().is_some() {
        app.chat_mut().push_system("已取消模型切换。".to_string());
    }
}

/// `/models:<p>/<id>` 或模型选择器确认：先选模型后选 effort——
///
/// - 目标模型支持推理：暂存为待切换模型并打开思考级别选择器（流程第二步）；
///   确认级别时一并应用切换，Esc 放弃整个切换。重选当前模型时不暂存，
///   级别选择器变为单纯的级别调整入口
/// - 目标模型不支持推理：直接切换（级别设置保留但随请求被忽略，
///   与配置文件 `reasoning` 同一口径）
///
/// 选择项为 `<provider>/<模型id>` 全形式；裸模型 id 在当前 provider 内解析。
pub(in crate::tui) fn select_model(app: &mut App, driver: &mut Driver, id: &str) {
    let selection = match ModelSelection::parse(id, Some(&driver.model.provider)) {
        Ok(selection) => selection,
        Err(error) => {
            app.warn(format!("切换模型失败：{error:#}"));
            return;
        }
    };
    match driver.models.resolve(&selection.provider, &selection.model) {
        Err(error) => app.warn(format!("切换模型失败：{error:#}")),
        Ok(model) if model.reasoning => {
            driver.pending_model = (!same_model(&model, &driver.model)).then_some(model);
            open_reasoning_picker(app, driver);
        }
        Ok(model) if same_model(&model, &driver.model) => {
            app.chat_mut()
                .push_system(format!("当前模型已是 {}（不支持思考）。", model.name));
        }
        Ok(model) => {
            if apply_model_switch(app, driver, model) {
                app.chat_mut().push_system(switch_notice(&driver.model));
            }
        }
    }
}

/// 同一模型判断：provider 与模型 id 均相同。
fn same_model(a: &Model, b: &Model) -> bool {
    a.provider == b.provider && a.id == b.id
}

/// 发送 SwitchModel job 并同步 driver/app 状态（状态栏徽标、上下文窗口）；
/// 成功返回 `true`，driver 已退出时告警并返回 `false`。
///
/// 跨 provider 时一并构造新连接实现（api_key 分层：环境变量 >
/// `providers.<名字>.api_key` > 平铺配置）；切换成功后把选择追加到
/// sqlite 配置表（下次启动的回退链顶端）。driver 串行处理任务，紧随的
/// 级别设置与 prompt 一定跑在新模型上。
fn apply_model_switch(app: &mut App, driver: &mut Driver, model: Model) -> bool {
    let provider = (model.provider != driver.model.provider).then(|| {
        let api_key = model::resolve_api_key(
            None,
            std::env::var(model::api_key_env(model.api)).ok().as_deref(),
            driver
                .models
                .provider_config(&model.provider)
                .and_then(|p| p.api_key.as_deref()),
            driver.models.config().and_then(|c| c.api_key.as_deref()),
        );
        ProviderSwitch {
            provider: model::build_provider(model.api, api_key.clone()),
            api_key,
        }
    });
    if driver
        .job_tx
        .send(DriverJob::SwitchModel(ModelSwitch {
            model: model.clone(),
            provider,
        }))
        .is_err()
    {
        app.warn("内部错误：agent 任务已退出，无法切换模型");
        return false;
    }
    persist_model_selection(driver, &model);
    let name = model.name.clone();
    let window = model.context_window;
    driver.model = model;
    app.set_model(name, window);
    true
}

/// 模型选择落库（config 表 append-only，最新行即下次启动的首选）。
///
/// 库不可用（启动已告警）时跳过；写失败只记日志不打断切换——
/// 下次启动的回退链只是少了这一条。
fn persist_model_selection(driver: &Driver, model: &Model) {
    let Some((store, _)) = &driver.session else {
        return;
    };
    let store = store.clone();
    let spec = current_selection(model).spec();
    tokio::spawn(async move {
        if let Err(error) = store
            .set_config(model::CONFIG_KEY_MODEL, &serde_json::Value::String(spec))
            .await
        {
            tracing::warn!(error = %error, "模型选择落库失败");
        }
    });
}

/// 切换成功的提示文本：`已切换模型为 <provider>/<模型id>（名称 · ctx 400k），
/// 对话上下文保留。`
fn switch_notice(model: &Model) -> String {
    format!("{}，对话上下文保留。", switched_part(model))
}

/// 提示文本的模型切换段：`已切换模型为 <provider>/<模型id>（名称 · ctx 400k）`；
/// 名称与模型 id 相同、窗口未知时省略对应段。
fn switched_part(model: &Model) -> String {
    use std::fmt::Write as _;
    let mut text = format!("已切换模型为 {}", current_selection(model).spec());
    let mut detail = String::new();
    if model.name != model.id {
        detail.push_str(&model.name);
    }
    if model.context_window > 0 {
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(
            detail,
            "ctx {}",
            widgets::format_tokens(model.context_window)
        );
    }
    if !detail.is_empty() {
        let _ = write!(text, "（{detail}）");
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

    /// `/models` 选择器行：id + 展示名 + 窗口，推理模型带标注，当前模型带标记，
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
