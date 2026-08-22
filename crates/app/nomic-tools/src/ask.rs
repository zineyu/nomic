//! `ask_user_question` 工具：向用户提问（单选 / 多选 / 填空）。
//!
//! 工具本身不直接碰终端：通过 [`QuestionSink`] 把问题交给宿主
//!（TUI 弹层 / print 模式的 stdin），阻塞等待回答后把结果回喂模型。
//! 单选/多选问题自动追加一个「自定义填写」选项（[`CUSTOM_OPTION`]），
//! 保证用户总能输入自己的答案，而不是被固定选项框死。
//!
//! 与用户的交互会阻塞本轮运行，工具声明 [`ExecutionMode::Sequential`]，
//! 避免同批次的并行工具调用在交互期间继续执行。

use std::sync::Arc;

use async_trait::async_trait;
use nomic_core::{AgentTool, ExecutionMode, ToolError, ToolResult, ToolUpdateCallback};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// 自动追加的自定义选项文案（单选/多选问题的最后一个选项）。
///
/// 选项列表里已含该文案时不重复追加；宿主据此识别自定义选项。
pub const CUSTOM_OPTION: &str = "✏️ 其他（自定义填写）";

/// 问题类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// 单选：用户选择一个选项（或自定义填写）
    #[default]
    SingleChoice,
    /// 多选：用户勾选若干选项（或自定义填写）
    MultipleChoice,
    /// 填空：用户自由输入文本
    FillIn,
}

/// 一次提问的完整描述（传给宿主渲染）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AskUserQuestion {
    /// 问题文本
    pub question: String,
    /// 问题类型
    pub kind: QuestionKind,
    /// 候选选项（单选/多选；填空为空）。单选/多选时末尾已自动追加
    /// [`CUSTOM_OPTION`] 自定义填写选项
    pub options: Vec<String>,
}

/// 用户回答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AskUserAnswer {
    /// 最终答案（单选 = 1 个选项文本；多选 = 若干选项文本；填空 =
    /// 输入文本；选择自定义填写时答案中包含自定义文本）
    pub answers: Vec<String>,
    /// 用户自定义填写的文本（未使用自定义选项时为 `None`；填空时等于
    /// 唯一答案，便于宿主与模型区分「自由输入」与「选项选择」）
    pub custom: Option<String>,
}

/// 问题宿：把 [`AskUserQuestion`] 展示给用户并阻塞等待回答。
///
/// 由宿主（TUI / print）实现；`cancel` 触发时实现方应尽快返回错误，
/// 避免运行被中断后工具仍挂起等待。
#[async_trait]
pub trait QuestionSink: Send + Sync + 'static {
    /// 展示问题并等待用户回答。
    async fn ask(
        &self,
        question: AskUserQuestion,
        cancel: CancellationToken,
    ) -> Result<AskUserAnswer, ToolError>;
}

/// `ask_user_question` 参数。
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AskUserQuestionParams {
    /// The question to ask the user
    pub question: String,
    /// Question type: single_choice (default) / multiple_choice / fill_in
    #[serde(default)]
    pub kind: QuestionKind,
    /// Candidate options (required for single_choice / multiple_choice; ignored for fill_in).
    /// A custom "fill in your own answer" option is always added automatically.
    #[serde(default)]
    pub options: Vec<String>,
}

const TOOL_NAME: &str = "ask_user_question";
const TOOL_DESCRIPTION: &str = "Ask the user a question and wait for their answer. \
         Supports single choice (single_choice), multiple choice (multiple_choice), \
         and free-form fill-in (fill_in). For single/multiple choice questions a custom \
         \"fill in your own answer\" option is always added automatically. Use this when \
         you need a decision, preference, or information that only the user can provide.";

/// `ask_user_question` 工具：经 [`QuestionSink`] 向用户提问并返回回答。
#[derive(Clone)]
pub struct AskUserQuestionTool {
    sink: Arc<dyn QuestionSink>,
}

impl std::fmt::Debug for AskUserQuestionTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskUserQuestionTool")
            .finish_non_exhaustive()
    }
}

impl AskUserQuestionTool {
    /// 绑定问题宿（TUI / print 实现）。
    pub fn new(sink: Arc<dyn QuestionSink>) -> Self {
        Self { sink }
    }
}

#[async_trait]
impl AgentTool for AskUserQuestionTool {
    type Params = AskUserQuestionParams;

    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn label(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        TOOL_DESCRIPTION
    }

    /// 交互阻塞本轮：声明串行，避免与同批次其他工具调用并发执行
    ///（交互弹层不应与其他工具的输出同时进行）。
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    async fn execute(
        &self,
        params: Self::Params,
        cancel: CancellationToken,
        _on_update: ToolUpdateCallback,
    ) -> Result<ToolResult, ToolError> {
        let question = build_question(&params)?;
        tracing::debug!(
            question = %question.question,
            kind = ?question.kind,
            options = question.options.len(),
            "asking user question"
        );
        let answer = self.sink.ask(question.clone(), cancel).await?;
        tracing::debug!(
            answers = answer.answers.len(),
            has_custom = answer.custom.is_some(),
            "user answered question"
        );
        Ok(ToolResult {
            details: Some(serde_json::json!({
                "question": question.question,
                "kind": question.kind,
                "options": question.options,
                "answers": answer.answers,
                "custom": answer.custom,
            })),
            ..ToolResult::text(render_answer(&question, &answer))
        })
    }
}

/// 校验参数并组装传给宿主的完整问题：单选/多选自动追加自定义选项
///（已含则不重复追加）。
fn build_question(params: &AskUserQuestionParams) -> Result<AskUserQuestion, ToolError> {
    let question = params.question.trim();
    if question.is_empty() {
        return Err(ToolError::new("question must not be empty"));
    }
    let mut options = params.options.clone();
    match params.kind {
        QuestionKind::FillIn => {
            // 填空没有候选选项：options 字段对模型是「忽略」语义
            options.clear();
        }
        QuestionKind::SingleChoice | QuestionKind::MultipleChoice => {
            if options.is_empty() {
                return Err(ToolError::new(
                    "options are required for single_choice / multiple_choice questions",
                ));
            }
            if !options.iter().any(|option| option == CUSTOM_OPTION) {
                options.push(CUSTOM_OPTION.to_string());
            }
        }
    }
    Ok(AskUserQuestion {
        question: question.to_string(),
        kind: params.kind,
        options,
    })
}

/// 回答渲染（回喂模型的文本契约）。
fn render_answer(question: &AskUserQuestion, answer: &AskUserAnswer) -> String {
    let kind = match question.kind {
        QuestionKind::SingleChoice => "single_choice",
        QuestionKind::MultipleChoice => "multiple_choice",
        QuestionKind::FillIn => "fill_in",
    };
    if answer.answers.len() == 1 {
        format!("User answered ({}): {}", kind, answer.answers[0])
    } else {
        let mut text = format!("User answered ({kind}):");
        for answer in &answer.answers {
            use std::fmt::Write as _;
            let _ = write!(text, "\n- {answer}");
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// 测试用问题宿：直接回传预设答案（校验工具侧契约）。
    struct StubSink(Arc<Mutex<Option<AskUserAnswer>>>);

    impl StubSink {
        fn answered(answer: AskUserAnswer) -> Self {
            Self(Arc::new(Mutex::new(Some(answer))))
        }
    }

    #[async_trait]
    impl QuestionSink for StubSink {
        async fn ask(
            &self,
            _question: AskUserQuestion,
            _cancel: CancellationToken,
        ) -> Result<AskUserAnswer, ToolError> {
            self.0
                .lock()
                .expect("lock")
                .take()
                .ok_or_else(|| ToolError::new("no preset answer"))
        }
    }

    fn tool(sink: Arc<dyn QuestionSink>) -> AskUserQuestionTool {
        AskUserQuestionTool::new(sink)
    }

    async fn execute(
        tool: &AskUserQuestionTool,
        params: AskUserQuestionParams,
    ) -> Result<ToolResult, ToolError> {
        tool.execute(params, CancellationToken::new(), Box::new(|_| {}))
            .await
    }

    #[test]
    fn single_choice_appends_custom_option_once() {
        let question = build_question(&AskUserQuestionParams {
            question: "语言？".to_string(),
            kind: QuestionKind::SingleChoice,
            options: vec!["Rust".to_string(), "Go".to_string()],
        })
        .expect("valid");
        assert_eq!(question.options, ["Rust", "Go", CUSTOM_OPTION]);
    }

    #[test]
    fn custom_option_not_duplicated_when_already_present() {
        let question = build_question(&AskUserQuestionParams {
            question: "语言？".to_string(),
            kind: QuestionKind::MultipleChoice,
            options: vec!["Rust".to_string(), CUSTOM_OPTION.to_string()],
        })
        .expect("valid");
        assert_eq!(
            question
                .options
                .iter()
                .filter(|o| *o == CUSTOM_OPTION)
                .count(),
            1
        );
    }

    #[test]
    fn fill_in_ignores_options() {
        let question = build_question(&AskUserQuestionParams {
            question: "邮箱？".to_string(),
            kind: QuestionKind::FillIn,
            options: vec!["a@b.c".to_string()],
        })
        .expect("valid");
        assert!(question.options.is_empty());
    }

    #[test]
    fn choice_without_options_rejected() {
        let error = build_question(&AskUserQuestionParams {
            question: "语言？".to_string(),
            kind: QuestionKind::SingleChoice,
            options: Vec::new(),
        })
        .expect_err("choice 必须给选项");
        assert!(error.to_string().contains("options are required"));
    }

    #[test]
    fn blank_question_rejected() {
        let error = build_question(&AskUserQuestionParams {
            question: "   ".to_string(),
            kind: QuestionKind::FillIn,
            options: Vec::new(),
        })
        .expect_err("空问题必须报错");
        assert!(error.to_string().contains("question must not be empty"));
    }

    #[tokio::test]
    async fn tool_returns_answer_with_structured_details() {
        let tool = tool(Arc::new(StubSink::answered(AskUserAnswer {
            answers: vec!["Rust".to_string()],
            custom: None,
        })));
        let result = execute(
            &tool,
            AskUserQuestionParams {
                question: "语言？".to_string(),
                kind: QuestionKind::SingleChoice,
                options: vec!["Rust".to_string(), "Go".to_string()],
            },
        )
        .await
        .expect("answer");
        let nomic_ai::UserContent::Text(text) = &result.content[0] else {
            panic!("expected text");
        };
        assert_eq!(text.text, "User answered (single_choice): Rust");
        let details = result.details.expect("details");
        assert_eq!(details["answers"][0], "Rust");
        assert_eq!(details["custom"], serde_json::Value::Null);
        assert_eq!(details["options"].as_array().expect("options").len(), 3);
    }

    #[tokio::test]
    async fn tool_returns_custom_text() {
        let tool = tool(Arc::new(StubSink::answered(AskUserAnswer {
            answers: vec!["Python".to_string()],
            custom: Some("Python".to_string()),
        })));
        let result = execute(
            &tool,
            AskUserQuestionParams {
                question: "语言？".to_string(),
                kind: QuestionKind::MultipleChoice,
                options: vec!["Rust".to_string(), "Go".to_string()],
            },
        )
        .await
        .expect("answer");
        let details = result.details.expect("details");
        assert_eq!(details["custom"], "Python");
    }

    #[tokio::test]
    async fn tool_renders_multiple_answers_as_list() {
        let tool = tool(Arc::new(StubSink::answered(AskUserAnswer {
            answers: vec!["Rust".to_string(), "Go".to_string()],
            custom: None,
        })));
        let result = execute(
            &tool,
            AskUserQuestionParams {
                question: "语言？".to_string(),
                kind: QuestionKind::MultipleChoice,
                options: vec!["Rust".to_string(), "Go".to_string(), "C".to_string()],
            },
        )
        .await
        .expect("answer");
        let nomic_ai::UserContent::Text(text) = &result.content[0] else {
            panic!("expected text");
        };
        assert_eq!(text.text, "User answered (multiple_choice):\n- Rust\n- Go");
    }
}
