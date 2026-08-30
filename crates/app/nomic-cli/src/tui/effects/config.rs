//! `config` 命令：sqlite 设置的查看与修改（ADR-0039）。
//!
//! 与 `nomic config` 子命令同构：复用 [`crate::config_cmd`] 的解析与执行
//! 逻辑，结果作为系统消息显示；写操作成功后经 [`ModelSwitcher`] 刷新
//! 模型解析器的设置快照，运行进程立即生效。

use super::model::ModelSwitcher;
use super::session::SessionBinding;
use crate::config_cmd::{self, ConfigCommand};
use crate::tui::app::App;

/// `config [子命令行]`：无参等价于 `list`。
pub(in crate::tui) async fn run(
    app: &mut App,
    session: &SessionBinding,
    switcher: &ModelSwitcher,
    args: &str,
) {
    let Some(store) = session.store() else {
        app.warn("session 库不可用，设置功能不可用");
        return;
    };
    let command = match parse(args) {
        Ok(command) => command,
        Err(error) => {
            app.chat_mut().push_system(format!(
                "{error}\n\n用法：config [list|get|set|unset|providers|models ...]（无参 = list）"
            ));
            return;
        }
    };
    match config_cmd::execute(&command, &store).await {
        Ok(output) => {
            // 写后刷新设置快照（读操作同样刷新无妨：查询冷路径，成本可忽略）
            switcher.reload_settings(&store).await;
            app.chat_mut().push_system(output);
        }
        Err(error) => app.warn(format!("{error:#}")),
    }
}

/// 解析 `config` 命令行原文：无参 → `list`；`set` 子命令的值段允许空格
/// 与 JSON 引号（取原文中 key 之后的剩余部分，不再按空白切分）。
fn parse(args: &str) -> anyhow::Result<ConfigCommand> {
    config_cmd::parse_args(&split_args(args))
}

fn split_args(args: &str) -> Vec<String> {
    let trimmed = args.trim();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.is_empty() {
        return vec!["list".to_string()];
    }
    if tokens[0] == "set" && tokens.len() > 2 {
        let key = tokens[1];
        let start = trimmed[3..]
            .find(key)
            .map_or(trimmed.len(), |i| i + 3 + key.len());
        let value = trimmed[start..].trim();
        return vec!["set".to_string(), key.to_string(), value.to_string()];
    }
    tokens.iter().map(|token| (*token).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_config_is_list() {
        assert_eq!(split_args(""), ["list"]);
    }

    #[test]
    fn set_value_keeps_spaces_and_quotes() {
        assert_eq!(
            split_args("set append_system 总是 用中文 回复"),
            ["set", "append_system", "总是 用中文 回复"]
        );
        assert_eq!(
            split_args(r#"set model_aliases {"smart": "openai/gpt-4o"}"#),
            ["set", "model_aliases", r#"{"smart": "openai/gpt-4o"}"#]
        );
    }

    #[test]
    fn other_subcommands_split_on_whitespace() {
        assert_eq!(
            split_args("providers set openai --base-url https://x"),
            ["providers", "set", "openai", "--base-url", "https://x"]
        );
        assert_eq!(split_args("get temperature"), ["get", "temperature"]);
    }

    #[test]
    fn parse_roundtrip_via_config_cmd() {
        assert!(matches!(
            parse("set temperature 0.7").expect("parse"),
            ConfigCommand::Set { .. }
        ));
        assert!(matches!(parse("").expect("bare"), ConfigCommand::List));
        assert!(parse("bogus").is_err());
    }
}
