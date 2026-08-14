//! 两步模型切换的状态机（`models`）：先选模型，推理模型再选思考级别
//! （第二步确认时一并应用切换，Esc 放弃整个切换）。
//!
//! 状态（当前模型/思考级别、待切换模型）与不变量集中于此：
//!
//! - 切换幂等：目标即当前模型时不产生切换（不暂存、不发任务）
//! - 级别幂等：级别未变化时不发设置任务
//! - job 顺序：级别设置经同一邮箱紧随 SwitchModel，必然跑在新模型上
//! - 跨 provider：切换时按启动同一口径构造新连接（api_key 分层）
//!
//! driver 只持有本实例并转发选择器结果；选择器 UI（行文本、预选）与
//! 选择落库接线在 `super`（effects::model）。流程级单测直接驱动本
//! 状态机（job 邮箱用 `mpsc::unbounded_channel` 即可构造），不起事件循环。

use nomic_ai::{Model, ThinkingLevel};
use tokio::sync::mpsc;

use super::reasoning_label;
use crate::model::{self, ModelChoice, ModelResolver, ModelSelection};
use crate::tui::driver::{DriverJob, ModelSwitch, ProviderSwitch};
use crate::tui::widgets;

/// 两步模型切换状态机：持有当前模型/思考级别与待切换模型。
pub(in crate::tui) struct ModelSwitcher {
    /// 运行时模型解析器（`models` 候选与切换，与启动同一分层口径）
    models: ModelResolver,
    /// 当前模型（应用切换后更新；选择器预选与切换幂等判断用）
    current: Model,
    /// 当前思考级别（级别选择器确认后更新；预选与级别幂等判断用）
    reasoning: Option<ThinkingLevel>,
    /// 待切换模型（流程第二步暂存）：模型选择器确认推理模型后、级别
    /// 选择器确认（应用切换）或 Esc（放弃切换）前持有；`None` 表示无
    /// 进行中的切换（含「重选当前推理模型仅调级别」场景）
    pending: Option<Model>,
}

/// 第一步（选择模型）的流转结果。
pub(super) enum Select {
    /// 目标支持推理：进入第二步（打开思考级别选择器）。
    /// 重选当前推理模型时不暂存，第二步变为单纯的级别调整入口
    AwaitLevel,
    /// 目标不支持推理：已直接切换（级别设置保留但随请求被忽略，
    /// 与配置文件 `reasoning` 同一口径）。附提示文本与待落库的选择 spec
    Switched { notice: String, persist: String },
    /// 目标即当前模型（不支持推理）：无变化
    AlreadyCurrent { notice: String },
    /// 选择项解析/候选校验失败或 driver 已退出：warn 文本
    Failed(String),
}

/// 第二步（确认思考级别）的流转结果。
pub(super) enum Confirm {
    /// 完成：附聊天区提示文本；应用了模型切换时带待落库的选择 spec
    /// （调用方据此更新状态栏徽标并落库）
    Done {
        notice: String,
        persist: Option<String>,
    },
    /// driver 已退出（job 发送失败）：warn 文本
    Failed(String),
}

impl ModelSwitcher {
    pub(in crate::tui) const fn new(
        models: ModelResolver,
        current: Model,
        reasoning: Option<ThinkingLevel>,
    ) -> Self {
        Self {
            models,
            current,
            reasoning,
            pending: None,
        }
    }

    /// 当前模型（切换成功后的状态栏徽标更新用）
    pub(super) const fn current(&self) -> &Model {
        &self.current
    }

    /// 当前思考级别（级别选择器预选用）
    pub(super) const fn reasoning(&self) -> Option<ThinkingLevel> {
        self.reasoning
    }

    /// `models` 候选列表与当前选择（选择器行构建与预选用）
    pub(super) fn candidates(&self) -> (ModelSelection, Vec<ModelChoice>) {
        let current = selection_of(&self.current);
        let choices = self.models.candidates(&current);
        (current, choices)
    }

    /// 第一步：选择模型（`models:<p>/<id>` 或模型选择器确认）。
    /// 选择项为 `<provider>/<模型id>` 全形式；裸模型 id 在当前 provider 内解析。
    pub(super) fn select(&mut self, id: &str, job_tx: &mpsc::UnboundedSender<DriverJob>) -> Select {
        let selection = match ModelSelection::parse(id, Some(&self.current.provider)) {
            Ok(selection) => selection,
            Err(error) => return Select::Failed(format!("切换模型失败：{error:#}")),
        };
        match self.models.resolve(&selection.provider, &selection.model) {
            Err(error) => Select::Failed(format!("切换模型失败：{error:#}")),
            Ok(model) if model.reasoning => {
                self.pending = (!same_model(&model, &self.current)).then_some(model);
                Select::AwaitLevel
            }
            Ok(model) if same_model(&model, &self.current) => Select::AlreadyCurrent {
                notice: format!("当前模型已是 {}（不支持思考）。", model.name),
            },
            Ok(model) => match self.apply(model, job_tx) {
                Ok(persist) => Select::Switched {
                    notice: switch_notice(&self.current),
                    persist,
                },
                Err(warn) => Select::Failed(warn),
            },
        }
    }

    /// 第二步：确认思考级别——先应用待切换模型，再设置级别；两者均未
    /// 变化时提示。
    ///
    /// 级别是请求参数，选择器只在目标模型支持推理时出现，因此设置必然
    /// 随请求生效（重选当前模型进入时当前模型即推理模型）。job 经同一
    /// 邮箱串行处理：级别设置一定排在模型切换之后。
    pub(super) fn confirm_level(
        &mut self,
        level: Option<ThinkingLevel>,
        job_tx: &mpsc::UnboundedSender<DriverJob>,
    ) -> Confirm {
        let mut parts: Vec<String> = Vec::new();
        let mut persist = None;
        if let Some(model) = self.pending.take() {
            match self.apply(model, job_tx) {
                Ok(spec) => {
                    parts.push(switched_part(&self.current));
                    persist = Some(spec);
                }
                Err(warn) => return Confirm::Failed(warn),
            }
        }
        if level != self.reasoning {
            if job_tx.send(DriverJob::SetReasoning(level)).is_err() {
                return Confirm::Failed("内部错误：agent 任务已退出，无法设置思考级别".to_string());
            }
            self.reasoning = level;
            parts.push(format!("思考级别设为 {}", reasoning_label(level)));
        }
        let notice = if parts.is_empty() {
            format!(
                "模型与思考级别均未变化（{}，级别 {}）。",
                self.current.name,
                reasoning_label(self.reasoning)
            )
        } else if persist.is_some() {
            format!("{}，对话上下文保留。", parts.join("，"))
        } else {
            format!("{}。", parts.join("，"))
        };
        Confirm::Done { notice, persist }
    }

    /// Esc 放弃切换：有进行中的待切换模型则丢弃并返回 true（调用方提示）；
    /// 模型与级别均不变。
    pub(super) fn cancel(&mut self) -> bool {
        self.pending.take().is_some()
    }

    /// 应用切换：发送 SwitchModel job（跨 provider 时按启动同一口径构造
    /// 新连接——api_key 分层：环境变量 > `providers.<名字>.api_key` >
    /// 平铺配置；CLI 的 `--api-key` 属于启动 provider，不参与运行时切换
    /// 分层）并把当前模型换为目标。成功返回待落库的选择 spec（config 表
    /// append-only，最新行即下次启动的首选）；driver 已退出返回 warn 文本。
    fn apply(
        &mut self,
        model: Model,
        job_tx: &mpsc::UnboundedSender<DriverJob>,
    ) -> Result<String, String> {
        let provider = (model.provider != self.current.provider).then(|| {
            let api_key = model::resolve_api_key(
                None,
                std::env::var(model::api_key_env(model.api)).ok().as_deref(),
                self.models
                    .provider_config(&model.provider)
                    .and_then(|p| p.api_key.as_deref()),
                self.models.config().and_then(|c| c.api_key.as_deref()),
            );
            ProviderSwitch {
                provider: model::build_provider(model.api, api_key.clone()),
                api_key,
            }
        });
        let spec = selection_of(&model).spec();
        if job_tx
            .send(DriverJob::SwitchModel(ModelSwitch {
                model: model.clone(),
                provider,
            }))
            .is_err()
        {
            return Err("内部错误：agent 任务已退出，无法切换模型".to_string());
        }
        self.current = model;
        Ok(spec)
    }
}

/// 同一模型判断：provider 与模型 id 均相同。
fn same_model(a: &Model, b: &Model) -> bool {
    a.provider == b.provider && a.id == b.id
}

/// 模型的选择项（`<provider>/<模型id>`）。
fn selection_of(model: &Model) -> ModelSelection {
    ModelSelection {
        provider: model.provider.clone(),
        model: model.id.clone(),
    }
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
    let mut text = format!("已切换模型为 {}", selection_of(model).spec());
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
    use clap::Parser as _;
    use nomic_ai::{Catalog, ThinkingLevel};
    use tokio::sync::mpsc;

    use super::{Confirm, ModelSwitcher, Select};
    use crate::Cli;
    use crate::model::ModelResolver;
    use crate::tui::driver::DriverJob;

    /// 裁剪的 models.dev api.json fixture：openai 两个非推理模型（同 provider
    /// 切换用），anthropic 一个推理模型（跨 provider 两步流用）。
    const MODELS_DEV_FIXTURE: &str = r#"{
        "openai": {
            "id": "openai",
            "models": {
                "gpt-4o": {
                    "id": "gpt-4o",
                    "name": "GPT-4o",
                    "reasoning": false,
                    "limit": { "context": 128000, "output": 16384 }
                },
                "gpt-4o-mini": {
                    "id": "gpt-4o-mini",
                    "name": "GPT-4o mini",
                    "reasoning": false,
                    "limit": { "context": 128000, "output": 16384 }
                }
            }
        },
        "anthropic": {
            "id": "anthropic",
            "models": {
                "claude-opus-4": {
                    "id": "claude-opus-4",
                    "name": "Claude Opus 4",
                    "reasoning": true,
                    "limit": { "context": 200000, "output": 32000 }
                }
            }
        }
    }"#;

    /// 初始状态：openai/gpt-4o + 思考级别 low；返回状态机与 job 邮箱两端。
    fn switcher() -> (
        ModelSwitcher,
        mpsc::UnboundedSender<DriverJob>,
        mpsc::UnboundedReceiver<DriverJob>,
    ) {
        let cli = Cli::parse_from(["nomic"]);
        let catalog = Catalog::parse(MODELS_DEV_FIXTURE).expect("catalog fixture");
        let models = ModelResolver::new(&cli, None, None, Some(catalog));
        let current = models.resolve("openai", "gpt-4o").expect("resolve");
        let (job_tx, job_rx) = mpsc::unbounded_channel();
        (
            ModelSwitcher::new(models, current, Some(ThinkingLevel::Low)),
            job_tx,
            job_rx,
        )
    }

    /// 收取一个 SwitchModel job，返回（模型 id, 是否跨 provider）。
    fn recv_switch(rx: &mut mpsc::UnboundedReceiver<DriverJob>) -> (String, bool) {
        match rx.try_recv() {
            Ok(DriverJob::SwitchModel(switch)) => {
                (switch.model.id.clone(), switch.provider.is_some())
            }
            other => panic!("应为 SwitchModel job：{}", job_name(&other)),
        }
    }

    /// 收取一个 SetReasoning job，返回级别。
    fn recv_reasoning(rx: &mut mpsc::UnboundedReceiver<DriverJob>) -> Option<ThinkingLevel> {
        match rx.try_recv() {
            Ok(DriverJob::SetReasoning(level)) => level,
            other => panic!("应为 SetReasoning job：{}", job_name(&other)),
        }
    }

    fn job_name(job: &Result<DriverJob, mpsc::error::TryRecvError>) -> &'static str {
        match job {
            Ok(DriverJob::Prompt(..)) => "Prompt",
            Ok(DriverJob::Compact(..)) => "Compact",
            Ok(DriverJob::Retry(..)) => "Retry",
            Ok(DriverJob::Inject(..)) => "Inject",
            Ok(DriverJob::Clear) => "Clear",
            Ok(DriverJob::Restore(..)) => "Restore",
            Ok(DriverJob::SwitchModel(..)) => "SwitchModel",
            Ok(DriverJob::SetReasoning(..)) => "SetReasoning",
            Err(mpsc::error::TryRecvError::Empty) => "<空>",
            Err(mpsc::error::TryRecvError::Disconnected) => "<断连>",
        }
    }

    /// 非推理模型：直接切换（发 SwitchModel + 待落库 spec），无第二步。
    #[test]
    fn select_non_reasoning_switches_immediately() {
        let (mut switcher, job_tx, mut rx) = switcher();
        let Select::Switched { notice, persist } = switcher.select("openai/gpt-4o-mini", &job_tx)
        else {
            panic!("非推理模型应直接切换");
        };
        assert_eq!(recv_switch(&mut rx), ("gpt-4o-mini".to_string(), false));
        assert_eq!(persist, "openai/gpt-4o-mini");
        assert!(
            notice.contains("已切换模型为 openai/gpt-4o-mini"),
            "{notice}"
        );
        assert!(notice.contains("对话上下文保留"), "{notice}");
        assert_eq!(switcher.current().id, "gpt-4o-mini");
        // recv_switch 断言同 provider 不构造新连接
    }

    /// 幂等：目标即当前模型时不切换（不发任务）；不支持思考时提示。
    #[test]
    fn select_current_non_reasoning_is_noop() {
        let (mut switcher, job_tx, mut rx) = switcher();
        let Select::AlreadyCurrent { notice } = switcher.select("gpt-4o", &job_tx) else {
            panic!("重选当前模型应无变化");
        };
        assert!(notice.contains("当前模型已是 GPT-4o"), "{notice}");
        assert!(rx.try_recv().is_err(), "不应发出任何 job");
        assert_eq!(switcher.current().id, "gpt-4o");
    }

    /// 两步流：选推理模型只暂存（不发任务、当前模型不变），确认级别时
    /// 先 SwitchModel 后 SetReasoning（同一邮箱保证顺序），跨 provider
    /// 携带新连接。
    #[test]
    fn reasoning_model_switches_on_level_confirm() {
        let (mut switcher, job_tx, mut rx) = switcher();
        let Select::AwaitLevel = switcher.select("anthropic/claude-opus-4", &job_tx) else {
            panic!("推理模型应进入第二步");
        };
        assert!(rx.try_recv().is_err(), "暂存阶段不应发出 job");
        assert_eq!(switcher.current().id, "gpt-4o", "确认前不切换");

        let Confirm::Done { notice, persist } =
            switcher.confirm_level(Some(ThinkingLevel::High), &job_tx)
        else {
            panic!("确认级别应完成切换");
        };
        assert_eq!(
            recv_switch(&mut rx),
            ("claude-opus-4".to_string(), true),
            "跨 provider 应携带新连接"
        );
        assert_eq!(
            recv_reasoning(&mut rx),
            Some(ThinkingLevel::High),
            "级别设置紧随模型切换"
        );
        assert_eq!(persist.as_deref(), Some("anthropic/claude-opus-4"));
        assert!(
            notice.contains("已切换模型为 anthropic/claude-opus-4"),
            "{notice}"
        );
        assert!(notice.contains("思考级别设为 high"), "{notice}");
        assert!(notice.ends_with("对话上下文保留。"), "{notice}");
        assert_eq!(switcher.current().id, "claude-opus-4");
        assert_eq!(switcher.reasoning(), Some(ThinkingLevel::High));
    }

    /// 级别幂等：重选当前推理模型（不暂存）且级别未变时，确认不发出
    /// 任何任务，提示「均未变化」。
    #[test]
    fn confirm_unchanged_level_is_noop() {
        let (mut switcher, job_tx, mut rx) = switcher();
        // 先切到推理模型
        let Select::AwaitLevel = switcher.select("anthropic/claude-opus-4", &job_tx) else {
            panic!("推理模型应进入第二步");
        };
        let Confirm::Done { .. } = switcher.confirm_level(Some(ThinkingLevel::High), &job_tx)
        else {
            panic!("确认级别应完成切换");
        };
        recv_switch(&mut rx);
        assert_eq!(recv_reasoning(&mut rx), Some(ThinkingLevel::High));

        // 重选当前推理模型：仍进入第二步但不暂存；级别未变 → 无任务
        let Select::AwaitLevel = switcher.select("anthropic/claude-opus-4", &job_tx) else {
            panic!("重选推理模型仍进入第二步");
        };
        let Confirm::Done { notice, persist } =
            switcher.confirm_level(Some(ThinkingLevel::High), &job_tx)
        else {
            panic!("确认级别应完成");
        };
        assert!(persist.is_none(), "未切换则无需落库");
        assert!(notice.contains("模型与思考级别均未变化"), "{notice}");
        assert!(rx.try_recv().is_err(), "幂等确认不应发出 job");
    }

    /// 仅调级别：重选当前推理模型改级别，只发 SetReasoning，不发 SwitchModel。
    #[test]
    fn level_only_adjustment_skips_model_switch() {
        let (mut switcher, job_tx, mut rx) = switcher();
        let Select::AwaitLevel = switcher.select("anthropic/claude-opus-4", &job_tx) else {
            panic!("推理模型应进入第二步");
        };
        let Confirm::Done { .. } = switcher.confirm_level(Some(ThinkingLevel::High), &job_tx)
        else {
            panic!("确认级别应完成切换");
        };
        recv_switch(&mut rx);
        recv_reasoning(&mut rx);

        let Select::AwaitLevel = switcher.select("anthropic/claude-opus-4", &job_tx) else {
            panic!("重选推理模型仍进入第二步");
        };
        let Confirm::Done { notice, persist } =
            switcher.confirm_level(Some(ThinkingLevel::Minimal), &job_tx)
        else {
            panic!("确认级别应完成");
        };
        assert_eq!(recv_reasoning(&mut rx), Some(ThinkingLevel::Minimal));
        assert!(rx.try_recv().is_err(), "仅调级别不应发 SwitchModel");
        assert!(persist.is_none());
        assert_eq!(notice, "思考级别设为 minimal。");
    }

    /// Esc 放弃：丢弃待切换模型，模型与级别均不变；放弃后的级别确认
    /// 不再应用切换。无进行中切换时放弃为无操作。
    #[test]
    fn esc_abandons_pending_switch() {
        let (mut switcher, job_tx, mut rx) = switcher();
        assert!(!switcher.cancel(), "无进行中切换时放弃为无操作");

        let Select::AwaitLevel = switcher.select("anthropic/claude-opus-4", &job_tx) else {
            panic!("推理模型应进入第二步");
        };
        assert!(switcher.cancel(), "有待切换模型时放弃生效");
        assert!(!switcher.cancel(), "放弃不可重复生效");

        // 放弃后确认级别：不再应用切换，仅按当前模型调级别
        let Confirm::Done { notice, persist } =
            switcher.confirm_level(Some(ThinkingLevel::High), &job_tx)
        else {
            panic!("确认级别应完成");
        };
        assert_eq!(recv_reasoning(&mut rx), Some(ThinkingLevel::High));
        assert!(rx.try_recv().is_err(), "放弃后不应再发 SwitchModel");
        assert!(persist.is_none());
        assert_eq!(notice, "思考级别设为 high。");
        assert_eq!(switcher.current().id, "gpt-4o", "放弃后模型不变");
    }

    /// 未知模型 / 未知 provider：解析失败，状态不变。
    #[test]
    fn select_unknown_model_fails_without_state_change() {
        let (mut switcher, job_tx, mut rx) = switcher();
        let Select::Failed(warn) = switcher.select("anthropic/claude-future", &job_tx) else {
            panic!("未知模型应失败");
        };
        assert!(warn.contains("切换模型失败"), "{warn}");
        assert!(rx.try_recv().is_err());
        assert_eq!(switcher.current().id, "gpt-4o");
    }
}
