# ADR-0040: 内部 URI 路由系统（internal-url router）

- 状态：草案
- 日期：2026-08-31

## 背景

nomic 目前只有一个内部 URI：`skill://`（ADR 无独立记录，实现于
`nomic-skills` 的 `SkillResolver` + read 工具特判）。随着会话产物
（截断输出、子 agent 结果、计划草稿）增多，「agent 可寻址的非普通文件
资源」需要统一抽象，而不是在 read/write 里继续堆 `strip_prefix` 特判。

oh-my-pi（pi 的下游分叉）用一套内部 URL 路由系统解决了同构问题：
统一解析器 + scheme 注册表 + `ProtocolHandler` 接口（resolve/write?/complete?）
+ 不可变标记。本 ADR 将该**架构**移植到 nomic，但不照搬其进程全局单例
与逐次调用上下文（`ResolveContext`）——那是为多会话宿主打补丁的形态；
nomic 的工具本就按会话构造（`with_skill_resolver` / `with_shared_base_dir`
构造期注入），因此 router 采用**每会话一个实例**、后端在构造期注入，
天然会话隔离，无需运行时修正层。

## 决策

### 新 crate：`crates/app/nomic-uri`

```
nomic-uri/
  src/
    parse.rs       # extract_uri_scheme / parse_internal_uri（正则优先 + 容错）
    router.rs      # UriRouter：scheme → handler 注册表，immutable 盖章
    handler.rs     # ProtocolHandler trait + UriResource + UriError
    selector.rs    # URI 尾挂选择器剥离（:N-M / :raw / :conflicts）
    handlers/
      skill.rs     # skill://    （迁入，复用 nomic_skills::SkillResolver）
      local.rs     # local://    （会话沙盒，唯一可写文件协议）
      artifact.rs  # artifact:// （截断工具输出落盘，见下）
      history.rs   # history://  （nomic-session SQLite 转录只读视图）
      agent.rs     # agent://    （子 agent 最终输出，AgentOutputSource trait 注入）
    conflict.rs    # conflict:// 不进 router：read 注册 / write 拼接的闭环
```

依赖方向：`nomic-uri` → `nomic-skills`、`nomic-session`；`nomic-tools`
→ `nomic-uri`；`nomic-core` 不依赖它（compaction 仅需一个 3 行的
`is_uri_scheme_path` 正则，就地添加）。`agent://` 用 `AgentOutputSource`
trait 反转依赖，避免 `nomic-uri` → `nomic-core`。

### 核心契约

```rust
pub trait ProtocolHandler: Send + Sync {
    fn scheme(&self) -> &'static str;
    /// 该协议产出的资源是否禁止 agent 编辑（router 盖章到 resource）
    fn immutable(&self) -> bool;
    async fn resolve(&self, url: &InternalUri) -> Result<UriResource, UriError>;
    /// 缺省 = 只读；router.write 对无此实现的 scheme 报 "read-only"
    async fn write(&self, _url: &InternalUri, _content: &str)
        -> Result<(), UriError> { Err(...) }
    /// 可选补全；必须快且本地
    fn complete(&self, _query: &str) -> Vec<UrlCompletion> { vec![] }
}

pub struct UriResource {
    pub url: String,
    pub content: String,
    pub content_type: ContentType,       // Markdown / Json / Plain
    pub source_path: Option<PathBuf>,    // 底层文件（grep/bash 用）
    pub immutable: bool,                 // router 从 handler 盖章
    pub is_directory: bool,
    pub notes: Vec<String>,
}
```

### 解析器要点（对齐 oh-my-pi `parse.ts`）

- `extract_uri_scheme`：接受层级形式（`scheme://`）与 opaque 形式
  （`scheme:rest`），三重防误判：单字母 scheme（Windows 盘符）、
  scheme 含 `.`（`foo.ts:50`）、尾巴匹配选择器语法（`Makefile:12`）。
- `parse_internal_uri`：正则先提取 scheme/host/path，再尝试严格解析；
  保留 `raw_host`（保大小写）、`raw_path`（保 `..`，containment 校验
  必须用原始形态）、`raw_href`（字节级原样）。host 段允许冒号
  （`skill://plugin:name` 不被当端口）。
- URI 选择器剥离采用**激进策略**（scheme 后的选择器形尾块一律剥离，
  交由统一解析报错）；文件路径维持现状（offset/limit 参数），不引入
  尾挂选择器。

### 六个协议的后端映射

| 协议 | 后端 | 可写 | immutable |
|---|---|---|---|
| `skill://<name>[/<path>]` | `nomic_skills::SkillResolver`（现有逻辑迁入） | 否 | 是 |
| `local://<path>` | `<artifacts_dir>/local/` 会话沙盒 | **是** | 否 |
| `artifact://<id>` | `ArtifactManager`：`<data_dir>/nomic/artifacts/<session_id>/<id>.<tool>.log` | 否 | 是 |
| `agent://<id>` | `AgentOutputSource`（multi_agent 的 wait_result 落库最终输出） | 否 | 是 |
| `history://[<id>]` | `nomic-session` 只读查询；无 id 列会话清单 | 否 | 是 |
| `conflict://<N>[/<side>]`、`conflict://*` | 会话级 `ConflictHistory`（read 注册 / write 拼接），**不进 router** | 写侧 | — |

`artifact://` 配套的 ArtifactManager：单调整数 ID（建目录时扫描现有
`*.log` 取 max+1，并发首分配共享同一初始化）；read/bash 输出超截断上限
时落盘并在翻页提示中给出 `artifact://<id>` 恢复路径。这同时解决
truncate 目前「截断即丢失」的问题。

### 安全边界（fail-closed）

- 文件型 handler（skill/local）统一走 `validate_relative_path`（词法拒绝
  绝对路径与 `..`）+ `path::absolute` 前缀检查 + `canonicalize` 后的
  containment 三层校验；dangling symlink 拒绝。
- `local://` 根在解析前创建；目标、父目录、根均 canonicalize 后比较。
- write/edit 对 router 可识别但无 `write` 实现的 URI 报 "read-only"；
  对形似 URI 但 router 不认识的写目标报错（防 `local:/x` 之类的
  塌缩拼写静默写成字面目录）。
- compaction 的 `extract_file_ops` 跳过一切 `scheme://` 路径：内部 URI
  是会话作用域资源，压缩后无法 re-ground，不进 `<files>` 摘要。

### 工具集成

- `read`：URI 判定 → 选择器剥离 → router.resolve → 内存分页
  （offset/limit 参数与 `:N-M` 尾挂选择器等价，二选一，混用报错）。
  `local://` 解析为真实文件后提升回普通文件管线（图片/目录等行为一致）。
- `write`：`conflict://` 短路 → router 可写 scheme（`local://`）→
  普通文件。hashline 锚点不存在于 nomic（edit 为 old/new 替换），
  immutable 的体现 = write/edit 拒绝 + 结果 details 标注。
- `grep`：URI 先 `resolve` 取 `source_path` 交 ripgrep；无 source_path
  的目录型资源拒绝（搜列表文本会误导）。
- `bash`：命令中的 `skill://` / `local://` / `artifact://` 展开为 shell
  转义的绝对路径；引号内的字面提及不展开。

### 与 ADR-0036（mem://）的关系

`mem://` 是 ADR-0036 草案的自有规划，**不在本 ADR 范围**。但 `mem://`
所设想的「页面与虚拟资源同构、read 统一浏览」正是本框架的注册点：
届时实现一个 `MemProtocolHandler` 注册进 router 即可，不需要再动
read/write。

## 非目标

- oh-my-pi 其余 9 个协议（vault/ssh/security/mcp/issue/pr/xd/omp/memory）：
  nomic 无对应子系统，不移植；框架保留注册点。
- TUI 自动补全 / OSC 8 超链接：`complete` 接口预留，UI 接线后续单独做。
- 文件路径的尾挂选择器（`file.rs:10-20`）：维持 offset/limit 参数。
- 进程全局 router 单例与跨会话资源发现（oh-my-pi 的 AgentRegistry 扫描）：
  nomic 每会话一个 router，无此需求。

## 后果

- read/write/grep/bash 的构造签名增加 `with_uri_router(Arc<UriRouter>)`；
  `default_tools_with_skills*` 系列函数改为接收 router 而非裸 resolver
  （skill resolver 成为 skill handler 的内部状态）。
- 每会话新增一个 artifacts 目录（lazy 创建）；会话删除策略不变
  （artifacts 目录随会话数据生命周期管理，后续可加 GC）。
- 系统提示词中 read 工具描述扩展为「文件或内部 URI（skill://、
  local://、artifact://、agent://、history://）」。
