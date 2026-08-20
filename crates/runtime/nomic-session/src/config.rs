//! 配置历史（`config` 表，append-only）：每次修改新增一行（不回写旧行），
//! 读取方从最新一行向最老一行逐步回退，直到没有可回退的行为止。
//! `session_id` 为 NULL 的行是全局配置，非 NULL 是会话级覆盖（web 多
//! session 各自的模型 / 思考级别），两类行用不同索引分开读取。

use nomic_ai::now_millis;

use crate::{SessionError, SessionStore, to_i64};

impl SessionStore {
    /// 追加一行**全局**配置值（`session_id` 为 NULL）：append-only，旧行保留
    /// 供回退。会话级配置用 [`Self::set_session_config`]，二者在 `session_id` 列上隔离。
    pub async fn set_config(
        &self,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), SessionError> {
        let payload = serde_json::to_string(value)?;
        sqlx::query("INSERT INTO config (\"key\", value, updated_at) VALUES (?, jsonb(?), ?)")
            .bind(key)
            .bind(payload)
            .bind(to_i64(now_millis()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 追加一行**会话级**配置值：append-only，按 `session_id` 隔离（web 多
    /// session 各自的模型 / 思考级别）。`session_id` 不存在时由外键约束拒绝。
    pub async fn set_session_config(
        &self,
        session_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), SessionError> {
        let payload = serde_json::to_string(value)?;
        sqlx::query(
            "INSERT INTO config (\"key\", value, updated_at, session_id) \
             VALUES (?, jsonb(?), ?, ?)",
        )
        .bind(key)
        .bind(payload)
        .bind(to_i64(now_millis()))
        .bind(session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 某**全局**配置键的全部历史值（最新在前），只读 `session_id IS NULL` 的行；
    /// 无法解析为 JSON 的行跳过（回退语义要求一行损坏不阻断更早的可用配置）。
    pub async fn config_history(&self, key: &str) -> Result<Vec<serde_json::Value>, SessionError> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT json(value) FROM config WHERE \"key\" = ? AND session_id IS NULL \
             ORDER BY id DESC",
        )
        .bind(key)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|text| serde_json::from_str(text).ok())
            .collect())
    }

    /// 某**会话级**配置键的全部历史值（最新在前），只读指定 `session_id` 的行；
    /// 跳过无法解析为 JSON 的行（语义同 [`Self::config_history`]）。
    pub async fn session_config_history(
        &self,
        session_id: &str,
        key: &str,
    ) -> Result<Vec<serde_json::Value>, SessionError> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT json(value) FROM config WHERE session_id = ? AND \"key\" = ? \
             ORDER BY id DESC",
        )
        .bind(session_id)
        .bind(key)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|text| serde_json::from_str(text).ok())
            .collect())
    }

    /// 最新一条可反序列化为 `T` 的**全局**配置值：逐行回退、跳过类型不符的行；
    /// 无可回退时返回 `None`。需要按领域语义继续校验的用 [`Self::config_history`]。
    pub async fn get_config<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, SessionError> {
        Ok(self
            .config_history(key)
            .await?
            .into_iter()
            .find_map(|value| serde_json::from_value(value).ok()))
    }

    /// 最新一条可反序列化为 `T` 的**会话级**配置值：回退语义同
    /// [`Self::get_config`]，但限定在指定 `session_id` 的覆盖链上。
    pub async fn get_session_config<T: serde::de::DeserializeOwned>(
        &self,
        session_id: &str,
        key: &str,
    ) -> Result<Option<T>, SessionError> {
        Ok(self
            .session_config_history(session_id, key)
            .await?
            .into_iter()
            .find_map(|value| serde_json::from_value(value).ok()))
    }
}
