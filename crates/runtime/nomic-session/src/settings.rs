//! 设置表（`providers` / `model_specs` / `settings`，ADR-0039）：「当前值」
//! 语义的 CRUD，替代 config.toml。与 append-only 的 `config` 表（`config`
//! 模块）互补：设置的修改是覆盖 / 删除（upsert / unset），不保留历史。
//!
//! 逐字段更新用补丁类型（[`ProviderPatch`] / [`ModelSpecPatch`]）：外层
//! `None` 不触碰该字段，`Some(None)` 清除，`Some(Some(_))` 设置——与
//! WebSocket JSON 协议里「字段缺失 / 显式 null / 有值」三态一一对应。

use nomic_ai::{ApiKind, ModelSpec, now_millis};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;

use crate::{SessionError, SessionStore, to_i64, to_u64};

/// provider 定义行（`providers` 表）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderRow {
    /// provider 名（主键）
    pub name: String,
    /// API 种类；`None` 时按名推断（anthropic / openai）
    pub api: Option<ApiKind>,
    /// API base URL
    pub base_url: Option<String>,
    /// API key
    pub api_key: Option<String>,
    /// 最后更新时间（Unix 毫秒）
    pub updated_at: u64,
}

/// `providers` 表的逐字段补丁：外层 `None` = 不更新，`Some(None)` = 清除，
/// `Some(Some(_))` = 设置（与 WS JSON 的 缺失 / null / 值 三态对应）。
// 双层 Option 即三态补丁语义本身，非误用
#[allow(clippy::option_option)]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderPatch {
    /// API 种类
    #[serde(default)]
    pub api: Option<Option<ApiKind>>,
    /// API base URL
    #[serde(default)]
    pub base_url: Option<Option<String>>,
    /// API key
    #[serde(default)]
    pub api_key: Option<Option<String>>,
}

/// 模型覆盖行（`model_specs` 表）：`spec` 中 `None` 字段 = 未覆盖，
/// 向下回退 models.dev / 中性兜底。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSpecRow {
    /// 所属 provider 名
    pub provider: String,
    /// 模型 id
    pub model_id: String,
    /// 规格覆盖（逐字段可选）
    #[serde(flatten)]
    pub spec: ModelSpec,
    /// 最后更新时间（Unix 毫秒）
    pub updated_at: u64,
}

/// `model_specs` 表的逐字段补丁（三态语义同 [`ProviderPatch`]）。
// 双层 Option 即三态补丁语义本身，非误用
#[allow(clippy::option_option)]
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModelSpecPatch {
    /// 展示名
    #[serde(default)]
    pub name: Option<Option<String>>,
    /// 是否支持推理/思考
    #[serde(default)]
    pub reasoning: Option<Option<bool>>,
    /// 是否支持图像输入（多模态）
    #[serde(default)]
    pub vision: Option<Option<bool>>,
    /// 上下文窗口 token 数
    #[serde(default)]
    pub context_window: Option<Option<u64>>,
    /// 最大输出 token 数
    #[serde(default)]
    pub max_tokens: Option<Option<u64>>,
    /// 每百万 token 费率：输入
    #[serde(default)]
    pub cost_input: Option<Option<f64>>,
    /// 每百万 token 费率：输出
    #[serde(default)]
    pub cost_output: Option<Option<f64>>,
    /// 每百万 token 费率：缓存读取
    #[serde(default)]
    pub cost_cache_read: Option<Option<f64>>,
    /// 每百万 token 费率：缓存写入
    #[serde(default)]
    pub cost_cache_write: Option<Option<f64>>,
}

impl ModelSpecPatch {
    /// 把补丁应用到既有规格上（三态合并）。
    fn apply_to(self, spec: &mut ModelSpec) {
        if let Some(value) = self.name {
            spec.name = value;
        }
        if let Some(value) = self.reasoning {
            spec.reasoning = value;
        }
        if let Some(value) = self.vision {
            spec.vision = value;
        }
        if let Some(value) = self.context_window {
            spec.context_window = value;
        }
        if let Some(value) = self.max_tokens {
            spec.max_tokens = value;
        }
        if let Some(value) = self.cost_input {
            spec.cost_input = value;
        }
        if let Some(value) = self.cost_output {
            spec.cost_output = value;
        }
        if let Some(value) = self.cost_cache_read {
            spec.cost_cache_read = value;
        }
        if let Some(value) = self.cost_cache_write {
            spec.cost_cache_write = value;
        }
    }
}

/// ApiKind ↔ TEXT 编码（与 serde snake_case 同名；显式 match 避免字符串化
/// 依赖 serde 实现细节）。
const fn api_to_str(api: ApiKind) -> &'static str {
    match api {
        ApiKind::AnthropicMessages => "anthropic_messages",
        ApiKind::OpenAiCompletions => "open_ai_completions",
    }
}

/// 解码 api 列（serde snake_case 同名）；未知值报数据损坏。
fn api_from_str(text: &str) -> Result<ApiKind, SessionError> {
    Ok(serde_json::from_value(serde_json::Value::String(
        text.to_string(),
    ))?)
}

/// bool ↔ INTEGER 编码（STRICT 表无 BOOLEAN 类型）。
fn bool_to_i64(value: bool) -> i64 {
    i64::from(value)
}

impl SessionStore {
    // ── providers ────────────────────────────────────────────────────────

    /// 全部 provider 定义（按名排序）。
    pub async fn list_providers(&self) -> Result<Vec<ProviderRow>, SessionError> {
        let rows = sqlx::query(
            "SELECT name, api, base_url, api_key, updated_at FROM providers ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(decode_provider_row).collect()
    }

    /// 读取单个 provider 定义；不存在返回 `None`。
    pub async fn get_provider(&self, name: &str) -> Result<Option<ProviderRow>, SessionError> {
        let row = sqlx::query(
            "SELECT name, api, base_url, api_key, updated_at FROM providers WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(decode_provider_row).transpose()
    }

    /// 新建或更新 provider（逐字段补丁，三态语义见 [`ProviderPatch`]）；
    /// 返回落库后的完整行。读改写包在一个事务里，避免并发写丢字段。
    pub async fn upsert_provider(
        &self,
        name: &str,
        patch: ProviderPatch,
    ) -> Result<ProviderRow, SessionError> {
        let mut tx = self.pool.begin().await?;
        let existing = sqlx::query(
            "SELECT name, api, base_url, api_key, updated_at FROM providers WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;
        let mut row = existing
            .as_ref()
            .map(decode_provider_row)
            .transpose()?
            .unwrap_or_else(|| ProviderRow {
                name: name.to_string(),
                api: None,
                base_url: None,
                api_key: None,
                updated_at: 0,
            });
        if let Some(value) = patch.api {
            row.api = value;
        }
        if let Some(value) = patch.base_url {
            row.base_url = value;
        }
        if let Some(value) = patch.api_key {
            row.api_key = value;
        }
        row.updated_at = now_millis();
        sqlx::query(
            "INSERT OR REPLACE INTO providers (name, api, base_url, api_key, updated_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&row.name)
        .bind(row.api.map(api_to_str))
        .bind(&row.base_url)
        .bind(&row.api_key)
        .bind(to_i64(row.updated_at))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// 删除 provider（其 `model_specs` 行级联清除）；返回是否有行被删除。
    pub async fn delete_provider(&self, name: &str) -> Result<bool, SessionError> {
        let result = sqlx::query("DELETE FROM providers WHERE name = ?")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    // ── model_specs ─────────────────────────────────────────────────────

    /// 全部模型覆盖（按 provider、模型 id 排序）。
    pub async fn list_model_specs(&self) -> Result<Vec<ModelSpecRow>, SessionError> {
        let rows = sqlx::query(
            "SELECT provider, model_id, name, reasoning, vision, context_window, max_tokens, \
             cost_input, cost_output, cost_cache_read, cost_cache_write, updated_at \
             FROM model_specs ORDER BY provider, model_id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(decode_model_spec_row).collect())
    }

    /// 新建或更新模型覆盖（逐字段补丁）；返回落库后的完整行。
    /// 所属 provider 必须存在（外键约束）。
    pub async fn upsert_model_spec(
        &self,
        provider: &str,
        model_id: &str,
        patch: ModelSpecPatch,
    ) -> Result<ModelSpecRow, SessionError> {
        let mut tx = self.pool.begin().await?;
        let existing = sqlx::query(
            "SELECT provider, model_id, name, reasoning, vision, context_window, max_tokens, \
             cost_input, cost_output, cost_cache_read, cost_cache_write, updated_at \
             FROM model_specs WHERE provider = ? AND model_id = ?",
        )
        .bind(provider)
        .bind(model_id)
        .fetch_optional(&mut *tx)
        .await?;
        let mut row = existing.as_ref().map_or_else(
            || ModelSpecRow {
                provider: provider.to_string(),
                model_id: model_id.to_string(),
                spec: ModelSpec::default(),
                updated_at: 0,
            },
            decode_model_spec_row,
        );
        patch.apply_to(&mut row.spec);
        row.updated_at = now_millis();
        sqlx::query(
            "INSERT OR REPLACE INTO model_specs (provider, model_id, name, reasoning, vision, \
             context_window, max_tokens, cost_input, cost_output, cost_cache_read, \
             cost_cache_write, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.provider)
        .bind(&row.model_id)
        .bind(&row.spec.name)
        .bind(row.spec.reasoning.map(bool_to_i64))
        .bind(row.spec.vision.map(bool_to_i64))
        .bind(row.spec.context_window.map(to_i64))
        .bind(row.spec.max_tokens.map(to_i64))
        .bind(row.spec.cost_input)
        .bind(row.spec.cost_output)
        .bind(row.spec.cost_cache_read)
        .bind(row.spec.cost_cache_write)
        .bind(to_i64(row.updated_at))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    /// 删除模型覆盖；返回是否有行被删除。
    pub async fn delete_model_spec(
        &self,
        provider: &str,
        model_id: &str,
    ) -> Result<bool, SessionError> {
        let result = sqlx::query("DELETE FROM model_specs WHERE provider = ? AND model_id = ?")
            .bind(provider)
            .bind(model_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    // ── settings（标量，upsert 语义）─────────────────────────────────────

    /// 写入标量设置（存在即覆盖）；值用 sqlite 原生 JSON 类型存储
    /// （同 `config` 表口径）。
    pub async fn set_setting(
        &self,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), SessionError> {
        tracing::debug!(key = %key, "setting scalar setting");
        let payload = serde_json::to_string(value)?;
        sqlx::query(
            "INSERT OR REPLACE INTO settings (\"key\", value, updated_at) \
             VALUES (?, jsonb(?), ?)",
        )
        .bind(key)
        .bind(payload)
        .bind(to_i64(now_millis()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 读取标量设置并反序列化为 `T`；键不存在或类型不符返回 `None`。
    pub async fn get_setting<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, SessionError> {
        let text =
            sqlx::query_scalar::<_, String>("SELECT json(value) FROM settings WHERE \"key\" = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        Ok(text.and_then(|text| serde_json::from_str(&text).ok()))
    }

    /// 删除标量设置；返回是否有行被删除。
    pub async fn unset_setting(&self, key: &str) -> Result<bool, SessionError> {
        let result = sqlx::query("DELETE FROM settings WHERE \"key\" = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// 全部标量设置（按键排序；无法解析为 JSON 的行跳过）。
    pub async fn list_settings(
        &self,
    ) -> Result<std::collections::BTreeMap<String, serde_json::Value>, SessionError> {
        let rows = sqlx::query("SELECT \"key\", json(value) FROM settings ORDER BY \"key\"")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let key: String = row.get("key");
                let text: String = row.get(1);
                serde_json::from_str(&text).ok().map(|value| (key, value))
            })
            .collect())
    }
}

/// 解码 providers 表行。
fn decode_provider_row(row: &sqlx::sqlite::SqliteRow) -> Result<ProviderRow, SessionError> {
    let api: Option<String> = row.get("api");
    Ok(ProviderRow {
        name: row.get("name"),
        api: api.map(|text| api_from_str(&text)).transpose()?,
        base_url: row.get("base_url"),
        api_key: row.get("api_key"),
        updated_at: to_u64(row.get("updated_at")),
    })
}

/// 解码 model_specs 表行。
fn decode_model_spec_row(row: &sqlx::sqlite::SqliteRow) -> ModelSpecRow {
    let reasoning: Option<i64> = row.get("reasoning");
    let vision: Option<i64> = row.get("vision");
    let context_window: Option<i64> = row.get("context_window");
    let max_tokens: Option<i64> = row.get("max_tokens");
    ModelSpecRow {
        provider: row.get("provider"),
        model_id: row.get("model_id"),
        spec: ModelSpec {
            name: row.get("name"),
            reasoning: reasoning.map(|v| v != 0),
            vision: vision.map(|v| v != 0),
            context_window: context_window.map(to_u64),
            max_tokens: max_tokens.map(to_u64),
            cost_input: row.get("cost_input"),
            cost_output: row.get("cost_output"),
            cost_cache_read: row.get("cost_cache_read"),
            cost_cache_write: row.get("cost_cache_write"),
        },
        updated_at: to_u64(row.get("updated_at")),
    }
}
