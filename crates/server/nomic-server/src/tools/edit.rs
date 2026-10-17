//! edit 工具：块替换编辑（非 replay-safe，重复执行有副作用）。
//!
//! 在文件内以 `old_text` 精确匹配唯一一处并替换为 `new_text`：
//! 匹配零处或多处均为工具错误（原因回灌模型，可修正后重试）。

use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use nomic_core::{Tool, ToolError, ToolName};

use crate::tools::resolve_path;

/// edit 输入。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditInput {
    /// 目标文件路径；相对路径基于会话工作目录解析。
    pub path: String,
    /// 被替换的原文，必须在文件中唯一匹配。
    pub old_text: String,
    /// 替换后的新文本。
    pub new_text: String,
}

/// edit 输出。
#[derive(Debug, Serialize, JsonSchema, PartialEq, Eq)]
pub struct EditOutput {
    /// 实际编辑的路径。
    pub path: String,
    /// 替换后文件内容的字节数。
    pub bytes: usize,
}

/// edit：唯一匹配的块替换编辑。
#[derive(Debug, Clone, Copy, Default)]
pub struct Edit;

impl Tool for Edit {
    type Input = EditInput;
    type Output = EditOutput;

    fn name(&self) -> ToolName {
        ToolName::new("edit")
    }

    fn description(&self) -> &'static str {
        "在文件内将唯一匹配 old_text 的文本块替换为 new_text；匹配零处或多处时返回工具错误。"
    }

    fn replay_safe(&self) -> bool {
        false
    }

    async fn call(
        &self,
        working_dir: &Path,
        input: Self::Input,
    ) -> Result<Self::Output, ToolError> {
        if input.old_text.is_empty() {
            return Err(ToolError::InvalidInput("old_text 为空".into()));
        }
        let path = resolve_path(working_dir, &input.path);
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|error| ToolError::Failed(format!("读取 {} 失败: {error}", path.display())))?;
        let occurrences = content.matches(&input.old_text).count();
        match occurrences {
            0 => {
                return Err(ToolError::Failed(format!(
                    "old_text 在 {} 中无匹配",
                    path.display()
                )));
            }
            1 => {}
            count => {
                return Err(ToolError::Failed(format!(
                    "old_text 在 {} 中匹配 {count} 处，必须唯一",
                    path.display()
                )));
            }
        }
        let updated = content.replacen(&input.old_text, &input.new_text, 1);
        tokio::fs::write(&path, &updated)
            .await
            .map_err(|error| ToolError::Failed(format!("写入 {} 失败: {error}", path.display())))?;
        Ok(EditOutput {
            path: path.display().to_string(),
            bytes: updated.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn 唯一匹配替换成功() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let output = Edit
            .call(
                dir.path(),
                EditInput {
                    path: "a.rs".into(),
                    old_text: "fn a() {}".into(),
                    new_text: "fn a() -> i32 { 1 }".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(output.bytes, "fn a() -> i32 { 1 }\nfn b() {}\n".len());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a() -> i32 { 1 }\nfn b() {}\n"
        );
    }

    #[tokio::test]
    async fn 零处匹配报工具错误() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();
        let error = Edit
            .call(
                dir.path(),
                EditInput {
                    path: "a.rs".into(),
                    old_text: "不存在".into(),
                    new_text: "x".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ToolError::Failed(message) if message.contains("无匹配")),
            "实际: {error}"
        );
        // 失败不得改动文件
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a() {}"
        );
    }

    #[tokio::test]
    async fn 多处匹配报工具错误() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let x = 1; let x = 2;").unwrap();
        let error = Edit
            .call(
                dir.path(),
                EditInput {
                    path: "a.rs".into(),
                    old_text: "let x".into(),
                    new_text: "let y".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ToolError::Failed(message) if message.contains("匹配 2 处")),
            "实际: {error}"
        );
    }

    #[tokio::test]
    async fn 空_old_text_为_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();
        let error = Edit
            .call(
                dir.path(),
                EditInput {
                    path: "a.rs".into(),
                    old_text: String::new(),
                    new_text: "x".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidInput(_)), "实际: {error}");
    }
}
