//! models.dev 模型目录：按模型 id 查询规格（展示名、推理能力、上下文/输出上限、费率）。
//!
//! 数据源为 <https://models.dev/api.json>（约 3MB，provider → models 嵌套结构）。
//! 拉取结果写磁盘缓存（`$XDG_CACHE_HOME/nomic/models-dev-api.json`，24h TTL）；
//! 网络失败时回退到过期缓存，缓存与网络均不可用时返回 `None`，由调用方落到
//! 内置默认值。本模块只提供「模型 id → 规格」查询，provider 与 base_url 永远
//! 来自用户配置，不经由 models.dev。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

/// api.json 端点。
const API_URL: &str = "https://models.dev/api.json";
/// 磁盘缓存有效期。
const CACHE_TTL: Duration = Duration::from_hours(24);
/// 网络拉取总超时（启动路径上的阻塞上限）。
///
/// api.json 约 3MB，慢网络下 3s 不够（实测部分网络 4s+），放宽到 10s 以
/// 保证首次拉取能写入缓存；命中缓存的启动不受此影响。
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// 模型规格：全部字段可选，缺省时由调用方继续向下层（models.dev / 内置默认）解析。
///
/// 同时作为 `config.toml` 中 `[providers.<名字>.models."<模型id>"]` 的反序列化
/// 目标，`deny_unknown_fields` 让配置中的拼写错误硬报错。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    /// 展示名
    pub name: Option<String>,
    /// 是否支持推理/思考
    pub reasoning: Option<bool>,
    /// 上下文窗口 token 数
    pub context_window: Option<u64>,
    /// 最大输出 token 数
    pub max_tokens: Option<u64>,
    /// 每百万 token 费率：输入
    pub cost_input: Option<f64>,
    /// 每百万 token 费率：输出
    pub cost_output: Option<f64>,
    /// 每百万 token 费率：缓存读取
    pub cost_cache_read: Option<f64>,
    /// 每百万 token 费率：缓存写入
    pub cost_cache_write: Option<f64>,
}

impl ModelSpec {
    /// 全部字段都有值时为 `true`（此时可跳过 models.dev 查询）。
    pub const fn is_complete(&self) -> bool {
        self.name.is_some()
            && self.reasoning.is_some()
            && self.context_window.is_some()
            && self.max_tokens.is_some()
            && self.cost_input.is_some()
            && self.cost_output.is_some()
            && self.cost_cache_read.is_some()
            && self.cost_cache_write.is_some()
    }

    /// 用下层来源填补本规格中缺失的字段（本层优先）。
    #[must_use]
    pub fn or_fill(&self, lower: &ModelSpec) -> ModelSpec {
        ModelSpec {
            name: self.name.clone().or_else(|| lower.name.clone()),
            reasoning: self.reasoning.or(lower.reasoning),
            context_window: self.context_window.or(lower.context_window),
            max_tokens: self.max_tokens.or(lower.max_tokens),
            cost_input: self.cost_input.or(lower.cost_input),
            cost_output: self.cost_output.or(lower.cost_output),
            cost_cache_read: self.cost_cache_read.or(lower.cost_cache_read),
            cost_cache_write: self.cost_cache_write.or(lower.cost_cache_write),
        }
    }
}

/// models.dev 目录（provider id → 模型 id → 规格）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    providers: HashMap<String, HashMap<String, ModelSpec>>,
}

/// api.json 中 provider 条目的反序列化目标（仅取需要的字段）。
#[derive(Deserialize)]
struct ProviderEntry {
    #[serde(default)]
    models: HashMap<String, ModelEntry>,
}

/// api.json 中模型条目的反序列化目标。
#[derive(Deserialize)]
struct ModelEntry {
    name: Option<String>,
    reasoning: Option<bool>,
    limit: Option<LimitEntry>,
    cost: Option<CostEntry>,
}

/// api.json 的 `limit` 子对象。
#[derive(Deserialize)]
struct LimitEntry {
    context: Option<u64>,
    output: Option<u64>,
}

/// api.json 的 `cost` 子对象。
#[derive(Deserialize)]
struct CostEntry {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

impl From<ModelEntry> for ModelSpec {
    fn from(entry: ModelEntry) -> Self {
        ModelSpec {
            name: entry.name,
            reasoning: entry.reasoning,
            context_window: entry.limit.as_ref().and_then(|l| l.context),
            max_tokens: entry.limit.as_ref().and_then(|l| l.output),
            cost_input: entry.cost.as_ref().and_then(|c| c.input),
            cost_output: entry.cost.as_ref().and_then(|c| c.output),
            cost_cache_read: entry.cost.as_ref().and_then(|c| c.cache_read),
            cost_cache_write: entry.cost.as_ref().and_then(|c| c.cache_write),
        }
    }
}

impl Catalog {
    /// 从 api.json 文本解析目录。
    ///
    /// 逐 provider、逐模型容错解析：单个脏条目跳过，不拖垮整个目录
    /// （models.dev  schema 不受本仓库控制，防御性解析换取启动稳定性）。
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        let raw: HashMap<String, serde_json::Value> = serde_json::from_str(text)?;
        let mut providers = HashMap::new();
        for (provider_id, value) in raw {
            let Ok(entry) = serde_json::from_value::<ProviderEntry>(value) else {
                continue;
            };
            let models = entry
                .models
                .into_iter()
                .map(|(id, entry)| (id, ModelSpec::from(entry)))
                .collect();
            providers.insert(provider_id, models);
        }
        Ok(Catalog { providers })
    }

    /// 按模型 id 查询规格：优先在 `provider_hint` 指向的 provider 下匹配（同一
    /// 模型 id 可能被多个 provider 以不同费率提供），找不到时全局扫描首个匹配。
    ///
    /// `provider_hint` 只影响匹配优先级；它本身永远来自用户配置而非 models.dev。
    pub fn lookup(&self, provider_hint: Option<&str>, model_id: &str) -> Option<&ModelSpec> {
        if let Some(models) = provider_hint.and_then(|hint| self.providers.get(hint))
            && let Some(spec) = models.get(model_id)
        {
            return Some(spec);
        }
        self.providers
            .values()
            .find_map(|models| models.get(model_id))
    }
}

/// 加载 models.dev 目录：新鲜缓存 → 网络拉取（成功则写缓存）→ 过期缓存 → `None`。
pub async fn load() -> Option<Catalog> {
    load_with(cache_path().ok().as_deref(), SystemTime::now(), fetch).await
}

/// `load` 的可测试内核：缓存路径、当前时间与网络拉取均可注入。
async fn load_with<F, Fut>(cache_path: Option<&Path>, now: SystemTime, fetch: F) -> Option<Catalog>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Option<String>>,
{
    if let Some(catalog) = cache_path.and_then(|path| read_fresh_cache(path, now)) {
        return Some(catalog);
    }
    if let Some(text) = fetch().await
        && let Ok(catalog) = Catalog::parse(&text)
    {
        if let Some(path) = cache_path {
            write_cache(path, &text);
        }
        return Some(catalog);
    }
    cache_path.and_then(read_stale_cache)
}

/// 拉取 api.json 文本；任何失败（建 client、网络、超时、非 2xx）都返回 `None`。
async fn fetch() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .ok()?;
    let response = client
        .get(API_URL)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    response.text().await.ok()
}

/// 读取新鲜（TTL 内）缓存；不存在、过期或解析失败都返回 `None`。
fn read_fresh_cache(path: &Path, now: SystemTime) -> Option<Catalog> {
    if !is_fresh(path, now) {
        return None;
    }
    read_stale_cache(path)
}

/// 读取缓存（不问新旧）；不存在或解析失败返回 `None`。
fn read_stale_cache(path: &Path) -> Option<Catalog> {
    let text = std::fs::read_to_string(path).ok()?;
    Catalog::parse(&text).ok()
}

/// 缓存文件是否在 TTL 内（按 mtime）。
fn is_fresh(path: &Path, now: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|mtime| now.duration_since(mtime).ok())
        .is_some_and(|age| age < CACHE_TTL)
}

/// 写缓存；任何 io 错误都忽略（缓存只是优化，失败不影响主流程）。
fn write_cache(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

/// 缓存路径：`$XDG_CACHE_HOME/nomic/models-dev-api.json`，
/// fallback `~/.cache/nomic/models-dev-api.json`（与 `config` 模块的手写 XDG 解析一致）。
fn cache_path() -> std::io::Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join("nomic").join("models-dev-api.json"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve cache path: neither XDG_CACHE_HOME nor HOME is set",
        )
    })?;
    Ok(PathBuf::from(home)
        .join(".cache")
        .join("nomic")
        .join("models-dev-api.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实 api.json 结构的裁剪 fixture：两个 provider，一个完整模型、
    /// 一个缺 cost/limit 的模型，外加一个坏 provider 条目（应被跳过）。
    const FIXTURE: &str = r#"{
        "anthropic": {
            "id": "anthropic",
            "name": "Anthropic",
            "models": {
                "claude-sonnet-4-5": {
                    "id": "claude-sonnet-4-5",
                    "name": "Claude Sonnet 4.5 (latest)",
                    "reasoning": true,
                    "tool_call": true,
                    "limit": { "context": 1000000, "output": 64000 },
                    "cost": { "input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75 }
                }
            }
        },
        "deepseek": {
            "id": "deepseek",
            "models": {
                "deepseek-chat": {
                    "id": "deepseek-chat",
                    "name": "DeepSeek Chat",
                    "reasoning": false
                }
            }
        },
        "broken": "not-a-provider-object"
    }"#;

    #[test]
    fn parse_extracts_spec_fields() {
        let catalog = Catalog::parse(FIXTURE).expect("parse");
        let spec = catalog
            .lookup(Some("anthropic"), "claude-sonnet-4-5")
            .expect("found");
        assert_eq!(spec.name.as_deref(), Some("Claude Sonnet 4.5 (latest)"));
        assert_eq!(spec.reasoning, Some(true));
        assert_eq!(spec.context_window, Some(1_000_000));
        assert_eq!(spec.max_tokens, Some(64_000));
        assert_eq!(spec.cost_input, Some(3.0));
        assert_eq!(spec.cost_output, Some(15.0));
        assert_eq!(spec.cost_cache_read, Some(0.3));
        assert_eq!(spec.cost_cache_write, Some(3.75));
    }

    #[test]
    fn parse_tolerates_missing_cost_and_limit() {
        let catalog = Catalog::parse(FIXTURE).expect("parse");
        let spec = catalog
            .lookup(Some("deepseek"), "deepseek-chat")
            .expect("found");
        assert_eq!(spec.name.as_deref(), Some("DeepSeek Chat"));
        assert_eq!(spec.reasoning, Some(false));
        assert_eq!(spec.context_window, None);
        assert_eq!(spec.cost_input, None);
    }

    #[test]
    fn lookup_prefers_provider_hint_then_falls_back_to_global_scan() {
        let catalog = Catalog::parse(FIXTURE).expect("parse");
        // hint 命中
        assert!(
            catalog
                .lookup(Some("anthropic"), "claude-sonnet-4-5")
                .is_some()
        );
        // hint 未命中 → 全局扫描
        assert!(catalog.lookup(Some("openai"), "deepseek-chat").is_some());
        // 无 hint → 全局扫描
        assert!(catalog.lookup(None, "deepseek-chat").is_some());
        // 完全不存在的模型
        assert!(catalog.lookup(None, "no-such-model").is_none());
    }

    #[test]
    fn spec_is_complete_only_when_all_fields_set() {
        let mut spec = ModelSpec {
            name: Some("x".to_string()),
            reasoning: Some(true),
            context_window: Some(1),
            max_tokens: Some(1),
            cost_input: Some(0.0),
            cost_output: Some(0.0),
            cost_cache_read: Some(0.0),
            cost_cache_write: Some(0.0),
        };
        assert!(spec.is_complete());
        spec.cost_cache_write = None;
        assert!(!spec.is_complete());
    }

    #[test]
    fn or_fill_keeps_upper_layer_values() {
        let upper = ModelSpec {
            max_tokens: Some(8192),
            ..ModelSpec::default()
        };
        let lower = ModelSpec {
            name: Some("name".to_string()),
            max_tokens: Some(64_000),
            ..ModelSpec::default()
        };
        let merged = upper.or_fill(&lower);
        assert_eq!(merged.name.as_deref(), Some("name"));
        assert_eq!(merged.max_tokens, Some(8192), "上层字段优先");
    }

    fn write_cache_file(dir: &tempfile::TempDir, text: &str) -> PathBuf {
        let path = dir.path().join("models-dev-api.json");
        std::fs::write(&path, text).expect("write cache");
        path
    }

    #[tokio::test]
    async fn fresh_cache_short_circuits_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_cache_file(&dir, FIXTURE);
        let catalog = load_with(Some(&path), SystemTime::now(), || async {
            panic!("新鲜缓存命中时不应发起网络请求");
        })
        .await;
        assert!(catalog.is_some());
    }

    #[tokio::test]
    async fn fetch_success_writes_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models-dev-api.json");
        let catalog = load_with(Some(&path), SystemTime::now(), || async {
            Some(FIXTURE.to_string())
        })
        .await;
        assert!(catalog.is_some());
        assert_eq!(std::fs::read_to_string(&path).expect("cache"), FIXTURE);
    }

    #[tokio::test]
    async fn fetch_failure_falls_back_to_stale_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_cache_file(&dir, FIXTURE);
        // now 取文件写入 48h 之后，使缓存过期
        let now = SystemTime::now() + Duration::from_hours(48);
        let catalog = load_with(Some(&path), now, || async { None }).await;
        assert!(catalog.is_some(), "网络失败时应回退到过期缓存");
    }

    #[tokio::test]
    async fn no_cache_and_fetch_failure_returns_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.json");
        let now = SystemTime::now();
        let catalog = load_with(Some(&path), now, || async { None }).await;
        assert!(catalog.is_none());
        let catalog = load_with(None, now, || async { None }).await;
        assert!(catalog.is_none());
    }
}
