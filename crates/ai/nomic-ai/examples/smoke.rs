//! 真实 provider 冒烟：一轮对话 + 一次工具调用。
//!
//! 手动运行，不进 CI：
//!
//! ```text
//! NOMIC_API_KEY=... NOMIC_MODEL=... \
//!   [NOMIC_BASE_URL=https://api.example/v1] \
//!   cargo run -p nomic-ai --example smoke
//! ```

use futures_util::StreamExt;
use nomic_ai::{ApiKey, ProviderConfig, RigAdapter};
use nomic_core::{
    LlmAdapter, LlmEvent, Message, MessageId, SessionId, ToolCall, ToolName, ToolResult, ToolSpec,
};
use serde_json::json;

/// 冒烟工具名。
const ECHO: &str = "echo";

fn env_var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("缺少环境变量 {name}"))
}

/// `echo` 工具声明。
fn echo_spec() -> ToolSpec {
    ToolSpec {
        name: ToolName::new(ECHO),
        description: "回显给定文本".into(),
        input_schema: json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        }),
        output_schema: json!({ "type": "string" }),
        replay_safe: true,
    }
}

/// 本地执行 `echo`；其余工具名属剧本错误。
fn execute(call: &ToolCall) -> ToolResult {
    if call.tool.as_str() != ECHO {
        return ToolResult::error(format!("未注册工具: {}", call.tool));
    }
    match call
        .arguments
        .get("text")
        .and_then(serde_json::Value::as_str)
    {
        Some(text) => ToolResult::ok(text.to_owned()),
        None => ToolResult::error("缺少 text 参数"),
    }
}

#[tokio::main]
async fn main() {
    let config = ProviderConfig::new(
        std::env::var("NOMIC_BASE_URL").ok(),
        ApiKey::parse(env_var("NOMIC_API_KEY")).expect("NOMIC_API_KEY 为空"),
        env_var("NOMIC_MODEL"),
    )
    .expect("provider 配置无效");
    let adapter = RigAdapter::openai(&config);
    let session = SessionId::new();
    let tools = vec![echo_spec()];
    let mut messages = vec![
        Message::System {
            id: MessageId::new(),
            text: "你是冒烟脚本。被要求回显时调用 echo 工具，然后总结结果。".into(),
        },
        Message::User {
            id: MessageId::new(),
            text: "请调用 echo 工具回显 \"hello nomic\"，然后告诉我结果。".into(),
        },
    ];

    // 最多两轮：首轮应请求工具调用，次轮给出最终答复
    for round in 1..=2 {
        println!("--- 第 {round} 轮 ---");
        let mut stream = adapter
            .respond(session, &messages, &tools)
            .await
            .expect("请求模型失败");
        let mut text = String::new();
        let mut calls = Vec::new();
        while let Some(event) = stream.next().await {
            match event.expect("流中途失败") {
                LlmEvent::Token(delta) => {
                    print!("{delta}");
                    text.push_str(&delta);
                }
                LlmEvent::ToolCallRequested(call) => calls.push(call),
            }
        }
        println!();
        if calls.is_empty() {
            return;
        }
        messages.push(Message::Assistant {
            id: MessageId::new(),
            text,
            tool_calls: calls.clone(),
        });
        for call in calls {
            let result = execute(&call);
            match &result {
                ToolResult::Ok { output } => println!("工具 {} 输出: {output}", call.tool),
                ToolResult::Error { message } | ToolResult::Rejected { reason: message } => {
                    println!("工具 {} 失败: {message}", call.tool);
                }
            }
            messages.push(Message::Tool {
                id: MessageId::new(),
                call_id: call.id,
                result,
            });
        }
    }
    eprintln!("两轮内模型未给出最终答复");
    std::process::exit(1);
}
