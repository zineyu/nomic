//! 工具 schema 方言归一化：把 schemars 生成的 draft 2020-12 schema
//! 改写为严格端点接受的子集。
//!
//! 目前唯一的方言是 MFJS（Moonshot Flavored JSON Schema，Moonshot / Kimi
//! 端点对 `tools.function.parameters` 的校验子集，规范见
//! <https://github.com/MoonshotAI/walle>）。相对 schemars 原生输出的差异：
//!
//! - `oneOf` 不在 MFJS 关键字集合内（`$ref` 指向含 `oneOf` 的定义会被
//!   端点误判为「无限递归」而 400）：纯标量 `const` 分支折叠为
//!   `type` + `enum`，其余 `oneOf` 改写为等价的 `anyOf`；
//! - `"type": ["T", "null"]` 多类型数组改写为 `anyOf` 分支
//!   （MFJS 的 `type` 只接受单个字符串，多类型与 `default` / `minimum` 等
//!   关键字同层会被拒绝）；
//! - 移除 `$schema` / `title` / `$comment` / `format` 注解
//!   （MFJS 明确不支持）；
//! - 根部 `$defs` 与内部 `$ref` 原样保留（MFJS 支持；递归定义经非必填
//!   属性或数组 `items` 引用时被视为可终止，已用 walle 校验器核实）。

use std::fmt::Write as _;

use serde_json::{Map, Value};

/// 工具 schema 方言（[`crate::providers::OpenAiCompat`] 的修补维度之一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolSchemaDialect {
    /// schemars 原生 draft 2020-12 输出（OpenAI 及多数兼容端点）。
    #[default]
    Standard,
    /// Moonshot Flavored JSON Schema 子集（Moonshot / Kimi 端点）。
    Mfjs,
}

/// 把 schema 归一化为 MFJS 子集（输入通常为 schemars 生成的参数 schema）。
pub fn normalize_mfjs(schema: &Value) -> Value {
    normalize_node(schema)
}

/// 递归归一化一个 schema 节点。
fn normalize_node(node: &Value) -> Value {
    let Value::Object(map) = node else {
        // 布尔 schema（true / false）与标量原样保留
        return node.clone();
    };

    let mut out = Map::with_capacity(map.len());
    for (key, value) in map {
        match key.as_str() {
            // MFJS 不支持的元数据 / 注解关键字
            "$schema" | "title" | "$comment" | "format" => {}
            // 这些关键字的值是「名字 -> schema」映射：名字不是关键字，只归一化值
            "properties" | "$defs" | "patternProperties" | "dependentSchemas" => {
                out.insert(key.clone(), normalize_schema_map(value));
            }
            // 子 schema
            "items"
            | "additionalProperties"
            | "contains"
            | "propertyNames"
            | "not"
            | "if"
            | "then"
            | "else"
            | "unevaluatedItems"
            | "unevaluatedProperties" => {
                out.insert(key.clone(), normalize_node(value));
            }
            // 子 schema 列表：oneOf / anyOf 优先折叠纯 const 分支；
            // oneOf 不在 MFJS 关键字集合内，折叠不了就改写为等价的 anyOf
            "oneOf" | "anyOf" => {
                if let Some(fold) = fold_const_branches(value) {
                    if let Some(branch_type) = fold.branch_type {
                        out.insert("type".to_string(), Value::String(branch_type));
                    }
                    out.insert("enum".to_string(), Value::Array(fold.enum_values));
                    fold_branch_docs(&mut out, map, &fold.branch_docs);
                } else {
                    merge_any_of(&mut out, normalize_schema_list(value));
                }
            }
            "allOf" | "prefixItems" => {
                out.insert(key.clone(), normalize_schema_list(value));
            }
            // 其余关键字（type / enum / const / default / description / required /
            // minimum …）的值不是 schema，原样保留
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }

    flatten_type_array(out)
}

/// 归一化「名字 -> schema」映射的每个值。
fn normalize_schema_map(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(name, schema)| (name.clone(), normalize_node(schema)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// 归一化 schema 列表的每个元素。
fn normalize_schema_list(value: &Value) -> Value {
    match value {
        Value::Array(list) => Value::Array(list.iter().map(normalize_node).collect()),
        other => other.clone(),
    }
}

/// 向输出节点合并 `anyOf` 分支：节点已带 `anyOf` 时追加（同一节点同时
/// 出现 `oneOf` 与 `anyOf` 的边角情形），否则新建。
fn merge_any_of(out: &mut Map<String, Value>, branches: Value) {
    let branches = match branches {
        Value::Array(list) => list,
        other => vec![other],
    };
    if let Some(existing) = out.get_mut("anyOf").and_then(Value::as_array_mut) {
        existing.extend(branches);
    } else {
        out.insert("anyOf".to_string(), Value::Array(branches));
    }
}

/// `oneOf` / `anyOf` 纯 `const` 分支的折叠结果。
struct ConstFold {
    /// 枚举值（分支的 `const`，保持顺序）
    enum_values: Vec<Value>,
    /// 分支的统一 `type`（各分支一致时；不一致或缺失则不折叠为此类型）
    branch_type: Option<String>,
    /// 分支上的逐值描述（`const` 字符串值 → 描述）
    branch_docs: Vec<(String, String)>,
}

/// `oneOf` / `anyOf` 分支全部是标量 `const`（字符串 / 数字；MFJS 的 `enum`
/// 不支持布尔与 null）时折叠。
///
/// 分支除 `const` 外只允许带 `type` / `description` / `title` 注解，
/// 否则不是可折叠的纯枚举形态。
fn fold_const_branches(value: &Value) -> Option<ConstFold> {
    let Value::Array(branches) = value else {
        return None;
    };
    if branches.is_empty() {
        return None;
    }
    let mut enum_values = Vec::with_capacity(branches.len());
    let mut branch_type: Option<Option<String>> = None;
    let mut branch_docs = Vec::new();
    for branch in branches {
        let Value::Object(branch) = branch else {
            return None;
        };
        let constant = branch.get("const")?;
        if !constant.is_string() && !constant.is_number() {
            return None;
        }
        if !branch
            .keys()
            .all(|key| matches!(key.as_str(), "const" | "type" | "description" | "title"))
        {
            return None;
        }
        let ty = branch
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match &branch_type {
            None => branch_type = Some(ty),
            Some(prev) if *prev != ty => return None,
            Some(_) => {}
        }
        if let (Value::String(name), Some(doc)) =
            (constant, branch.get("description").and_then(Value::as_str))
        {
            branch_docs.push((name.clone(), doc.to_string()));
        }
        enum_values.push(constant.clone());
    }
    Some(ConstFold {
        enum_values,
        branch_type: branch_type.flatten(),
        branch_docs,
    })
}

/// 把分支上的逐值描述折叠进字段描述（`value: 描述` 逐行追加），
/// 避免折叠 `oneOf` 后丢失枚举值的语义说明。
fn fold_branch_docs(
    out: &mut Map<String, Value>,
    original: &Map<String, Value>,
    docs: &[(String, String)],
) {
    if docs.is_empty() {
        return;
    }
    let mut description = original
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    for (name, doc) in docs {
        if !description.is_empty() {
            description.push('\n');
        }
        let _ = write!(description, "- `{name}`: {doc}");
    }
    out.insert("description".to_string(), Value::String(description));
}

/// `"type": ["T", …]` 多类型数组改写为 `anyOf` 分支（MFJS 的 `type`
/// 只接受单个字符串；其余关键字提升到 `anyOf` 同层）。
fn flatten_type_array(mut out: Map<String, Value>) -> Value {
    let Some(types) = out.get("type") else {
        return Value::Object(out);
    };
    let Value::Array(types) = types else {
        return Value::Object(out);
    };
    if out.contains_key("anyOf") {
        return Value::Object(out);
    }
    let branches: Vec<Value> = types
        .iter()
        .filter_map(|ty| ty.as_str().map(|ty| serde_json::json!({ "type": ty })))
        .collect();
    if branches.len() != types.len() {
        return Value::Object(out);
    }
    out.remove("type");
    let mut wrapped = Map::with_capacity(out.len() + 1);
    wrapped.insert("anyOf".to_string(), Value::Array(branches));
    wrapped.append(&mut out);
    Value::Object(wrapped)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// schemars 对「带文档注释的单元枚举」的输出形态（ask_user_question 的
    /// `kind` 字段）：`$ref` 指向含 `oneOf` 的定义——Moonshot 端点会把它
    /// 误判为无限递归而 400。
    fn ask_like_schema() -> Value {
        json!({
            "$defs": {
                "QuestionKind": {
                    "description": "问题类型。",
                    "oneOf": [
                        { "const": "single_choice", "description": "单选", "type": "string" },
                        { "const": "fill_in", "description": "填空", "type": "string" }
                    ]
                }
            },
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "description": "参数。",
            "properties": {
                "kind": {
                    "$ref": "#/$defs/QuestionKind",
                    "default": "single_choice",
                    "description": "Question type"
                },
                "question": { "title": "Question", "type": "string" }
            },
            "required": ["question"],
            "title": "AskParams",
            "type": "object"
        })
    }

    #[test]
    fn const_one_of_folds_to_enum() {
        let normalized = normalize_mfjs(&ask_like_schema());
        assert_eq!(
            normalized["$defs"]["QuestionKind"],
            json!({
                "description": "问题类型。\n- `single_choice`: 单选\n- `fill_in`: 填空",
                "type": "string",
                "enum": ["single_choice", "fill_in"]
            })
        );
        // $ref 与 sibling 保留（MFJS 支持根部 $defs + 内部 $ref）
        assert_eq!(
            normalized["properties"]["kind"]["$ref"],
            "#/$defs/QuestionKind"
        );
        assert_eq!(normalized["properties"]["kind"]["default"], "single_choice");
    }

    #[test]
    fn unsupported_annotations_removed() {
        let normalized = normalize_mfjs(&ask_like_schema());
        assert!(normalized.get("$schema").is_none());
        assert!(normalized.get("title").is_none());
        assert!(normalized["properties"]["question"].get("title").is_none());
    }

    #[test]
    fn type_array_flattens_to_any_of() {
        let schema = json!({
            "properties": {
                "id": {
                    "default": null,
                    "description": "optional id",
                    "type": ["string", "null"]
                }
            },
            "type": "object"
        });
        let normalized = normalize_mfjs(&schema);
        assert_eq!(
            normalized["properties"]["id"],
            json!({
                "anyOf": [{ "type": "string" }, { "type": "null" }],
                "default": null,
                "description": "optional id"
            })
        );
    }

    #[test]
    fn recursive_defs_and_refs_preserved() {
        // todo_write 的 TodoItemInput 形态：经数组 items 自引用
        let schema = json!({
            "$defs": {
                "Item": {
                    "properties": {
                        "children": {
                            "items": { "$ref": "#/$defs/Item" },
                            "type": "array"
                        },
                        "title": { "type": "string" }
                    },
                    "required": ["title"],
                    "type": "object"
                }
            },
            "properties": {
                "todos": { "items": { "$ref": "#/$defs/Item" }, "type": "array" }
            },
            "type": "object"
        });
        let normalized = normalize_mfjs(&schema);
        assert_eq!(
            normalized["$defs"]["Item"]["properties"]["children"]["items"]["$ref"],
            "#/$defs/Item"
        );
        assert_eq!(
            normalized["properties"]["todos"]["items"]["$ref"],
            "#/$defs/Item"
        );
    }

    #[test]
    fn non_const_one_of_rewritten_to_any_of() {
        // oneOf 不在 MFJS 关键字集合内：非 const 分支的 oneOf 改写为 anyOf
        let schema = json!({
            "properties": {
                "qs": {
                    "oneOf": [
                        { "type": "string" },
                        { "type": "array", "items": { "type": "string" } }
                    ]
                }
            },
            "type": "object"
        });
        let normalized = normalize_mfjs(&schema);
        assert_eq!(
            normalized["properties"]["qs"],
            json!({
                "anyOf": [
                    { "type": "string" },
                    { "type": "array", "items": { "type": "string" } }
                ]
            })
        );
    }

    #[test]
    fn property_named_like_keyword_not_stripped() {
        // properties 下的属性名不是 schema 关键字：名为 format / title 的
        // 属性必须保留
        let schema = json!({
            "properties": {
                "format": { "type": "string" },
                "title": { "type": "string" }
            },
            "type": "object"
        });
        let normalized = normalize_mfjs(&schema);
        assert_eq!(
            normalized["properties"]["format"],
            json!({ "type": "string" })
        );
        assert_eq!(
            normalized["properties"]["title"],
            json!({ "type": "string" })
        );
    }

    #[test]
    fn bool_const_branches_not_folded_to_enum() {
        // MFJS 的 enum 不支持布尔：不折叠为 enum，但 oneOf 仍改写为 anyOf
        let schema = json!({
            "properties": {
                "flag": {
                    "oneOf": [
                        { "const": true, "type": "boolean" },
                        { "const": false, "type": "boolean" }
                    ]
                }
            }
        });
        let normalized = normalize_mfjs(&schema);
        assert!(normalized["properties"]["flag"].get("oneOf").is_none());
        assert!(normalized["properties"]["flag"].get("enum").is_none());
        assert_eq!(
            normalized["properties"]["flag"]["anyOf"],
            json!([
                { "const": true, "type": "boolean" },
                { "const": false, "type": "boolean" }
            ])
        );
    }
}
