//! grep/find 共用的 fff 常驻索引管理：按搜索根懒创建 `FilePicker`
//!（后台扫描 + 文件系统 watcher），进程内复用，超出容量按 LRU 逐出。
//!
//! 与旧的逐次全盘遍历不同，fff 的索引常驻内存：首次调用等待初始扫描
//!（有超时，超时后以部分索引继续），之后的搜索全部命中热索引；
//! watcher 让 agent 经 write/edit 产生的文件改动即时反映到索引里。
//!
//! frecency 持久化（LMDB）未启用：`SharedFrecency::default()` 不建库，
//! 排序退化为 git status / 模糊评分（fff-mcp 在未配置 db 路径时亦然）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use fff_search::{FFFMode, FilePicker, FilePickerOptions, SharedFilePicker, SharedFrecency};
use nomic_core::ToolError;

/// 进程内最多同时保活的索引数，超出时逐出最久未用的（取消其后台线程）。
/// 实际会话通常只有 1-2 个搜索根；上限是为根频繁切换的拓展场景兑底。
const MAX_PICKERS: usize = 16;
/// 首次扫描的最长等待；fff 扫描远快于此，超时只是兜底（以部分索引继续）。
const INITIAL_SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// 一条索引记录。
struct PickerEntry {
    picker: SharedFilePicker,
    /// 初始扫描已等待过：之后的 watcher 触发重扫不再阻塞搜索调用。
    initial_scan_waited: Arc<AtomicBool>,
    last_used: Instant,
}

#[derive(Default)]
struct Registry {
    /// key = 规范化（canonical）后的搜索根绝对路径。
    entries: HashMap<PathBuf, PickerEntry>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn lock_registry() -> std::sync::MutexGuard<'static, Registry> {
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// 取 `root` 对应的共享索引：不存在则创建并等待首次扫描完成。
///
/// `root` 必须是已存在的目录；调用方负责校验（单文件根由调用方特判）。
pub fn picker_for(root: &Path) -> Result<SharedFilePicker, ToolError> {
    let key = std::fs::canonicalize(root)
        .map_err(|e| ToolError::new(format!("Cannot resolve path {}: {e}", root.display())))?;
    let (picker, initial_scan_waited) = {
        let mut reg = lock_registry();
        let handles = if let Some(entry) = reg.entries.get_mut(&key) {
            entry.last_used = Instant::now();
            (entry.picker.clone(), Arc::clone(&entry.initial_scan_waited))
        } else {
            let picker = spawn_picker(&key)?;
            let entry = PickerEntry {
                picker,
                initial_scan_waited: Arc::new(AtomicBool::new(false)),
                last_used: Instant::now(),
            };
            let handles = (entry.picker.clone(), Arc::clone(&entry.initial_scan_waited));
            reg.entries.insert(key, entry);
            evict_lru(&mut reg);
            handles
        };
        drop(reg);
        handles
    };

    // 仅首次等待初始扫描；之后的调用直接命中索引（后台重扫不阻塞）。
    if !initial_scan_waited.load(Ordering::Acquire) {
        let done = picker.wait_for_scan(INITIAL_SCAN_TIMEOUT);
        if !done {
            tracing::warn!(
                timeout_ms = INITIAL_SCAN_TIMEOUT.as_millis(),
                "fff initial scan not finished; searching partial index"
            );
        }
        initial_scan_waited.store(true, Ordering::Release);
    }
    Ok(picker)
}

/// 创建 picker 并 spawn 后台扫描 + watcher。
fn spawn_picker(root: &Path) -> Result<SharedFilePicker, ToolError> {
    let picker = SharedFilePicker::default();
    FilePicker::new_with_shared_state(
        picker.clone(),
        SharedFrecency::default(),
        FilePickerOptions {
            base_path: root.display().to_string(),
            mode: FFFMode::Ai,
            // 扫描后构建内容索引（bigram 预过滤），加速后续 grep。
            enable_content_indexing: true,
            // watcher 保持索引随文件系统变更新鲜。
            watch: true,
            ..Default::default()
        },
    )
    .map_err(|e| ToolError::new(format!("Cannot index {}: {e}", root.display())))?;
    Ok(picker)
}

/// 超出容量时逐出最久未用的索引（取消后台扫描/watcher 线程后丢弃）。
fn evict_lru(reg: &mut Registry) {
    while reg.entries.len() > MAX_PICKERS {
        let Some(oldest) = reg
            .entries
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        if let Some(entry) = reg.entries.remove(&oldest) {
            tracing::debug!(root = %oldest.display(), "evicting fff index");
            entry.picker.cancel();
        }
    }
}

/// 是否为 VCS 内部路径（`.git/`、`.jj/` 的任意层级组件）。
///
/// fff 只排除 `.git`；jj 仓库的 `.jj/repo` 含大量 git 对象文件，
/// 在 git 仓库内 fff 会索引隐藏目录，必须在输出侧过滤。
pub fn is_vcs_internal_path(relative_path: &str) -> bool {
    relative_path
        .split('/')
        .any(|component| component == ".git" || component == ".jj")
}
