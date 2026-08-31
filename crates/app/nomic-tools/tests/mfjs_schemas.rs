//! 全部内置工具的参数 schema 归一化为 MFJS（Moonshot Flavored JSON Schema）
//! 子集后的结构不变量测试。
//!
//! 背景：Moonshot / Kimi 端点用 walle 校验器严格检查
//! `tools.function.parameters`，schemars 原生输出中的 `oneOf`、多类型
//! `type` 数组、`$schema` / `title` / `format` 注解会被 400 拒绝。
//! 本测试锁定「归一化后的 schema 落在 MFJS 子集内」这一契约（归一化前
//! 的形态曾逐工具用 walle CLI 复核：9 个内置工具 6 个被拒，归一化后全部
//! 通过；此处不依赖外部校验器，只断言等价的结构不变量）。

use std::sync::Arc;

use serde_json::Value;

struct NullSink;

#[async_trait::async_trait]
impl nomic_tools::QuestionSink for NullSink {
    async fn ask(
        &self,
        _question: nomic_tools::AskUserQuestion,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<nomic_tools::AskUserAnswer, nomic_core::ToolError> {
        Err(nomic_core::ToolError::new("null sink"))
    }
}

/// 递归断言一个 schema 节点落在 MFJS 子集内（`path` 用于断言失败定位）。
fn assert_mfjs_node(node: &Value, path: &str, is_root: bool) {
    let Value::Object(map) = node else {
        return; // 布尔 schema
    };
    for (key, value) in map {
        match key.as_str() {
            // MFJS 不支持的注解 / 组合关键字
            "$schema" | "title" | "$comment" | "format" | "oneOf" => {
                panic!("{path}: MFJS 不支持的关键字 {key:?}");
            }
            // $defs 只允许出现在根部
            "$defs" => {
                assert!(is_root, "{path}: $defs 只允许在 schema 根部");
                assert_schema_map(value, &format!("{path}.$defs"));
            }
            // $ref 只允许内部引用
            "$ref" => {
                let reference = value.as_str().expect("$ref 必须是字符串");
                assert!(
                    reference == "#" || reference.starts_with("#/$defs/"),
                    "{path}: $ref 只支持内部引用，得到 {reference:?}"
                );
            }
            "properties" | "patternProperties" | "dependentSchemas" => {
                assert_schema_map(value, &format!("{path}.{key}"));
            }
            "type" => {
                assert!(
                    value.is_string(),
                    "{path}: type 必须是单个字符串，得到 {value}"
                );
            }
            "enum" => {
                let values = value.as_array().expect("enum 必须是数组");
                assert!(
                    values.iter().all(|v| v.is_string() || v.is_number()),
                    "{path}: enum 元素必须是字符串或数字，得到 {values:?}"
                );
            }
            "items"
            | "additionalProperties"
            | "contains"
            | "propertyNames"
            | "not"
            | "if"
            | "then"
            | "else" => {
                if value.is_object() {
                    assert_mfjs_node(value, &format!("{path}.{key}"), false);
                }
            }
            "anyOf" | "allOf" | "prefixItems" => {
                for (index, branch) in value.as_array().expect("子 schema 列表").iter().enumerate()
                {
                    assert_mfjs_node(branch, &format!("{path}.{key}[{index}]"), false);
                }
            }
            // required / description / default / const / minimum … 非 schema 值
            _ => {}
        }
    }
}

/// 「名字 -> schema」映射：名字不是关键字，逐个检查值。
fn assert_schema_map(value: &Value, path: &str) {
    let map = value.as_object().expect("schema 映射必须是对象");
    for (name, schema) in map {
        assert_mfjs_node(schema, &format!("{path}.{name}"), false);
    }
}

#[test]
fn all_builtin_tool_schemas_normalize_to_mfjs_subset() {
    let tools = nomic_tools::default_tools(nomic_tools::TodoStore::new(), Arc::new(NullSink));
    assert!(tools.len() >= 9, "默认工具集应包含全部内置工具");
    for tool in &tools {
        let definition = tool.definition();
        let normalized = nomic_ai::providers::normalize_mfjs(&definition.parameters);
        assert_mfjs_node(&normalized, &definition.name, true);
    }
}
