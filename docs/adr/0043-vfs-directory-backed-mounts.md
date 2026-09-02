# ADR-0043: VFS 的目录化挂载语义（一切皆目录）

- 状态：草案
- 日期：2026-09-02
- 修订：ADR-0042（内部 URI 的 VFS 化）

## 背景

ADR-0042 把后端契约升级为 `stat/read/list/write` 的文件系统操作集，
但语义层仍有三处缺口：

1. **`source_path` 是 `Option`**：无文件系统背书的 VFS（ADR-0040 规划
   的 `history://`、`agent://` 等）让 grep/bash/find 一律报「virtual
   resource not backed by a filesystem path」——「一切皆文件」的承诺
   恰恰在最有价值的场景（如 grep 会话历史）断裂。
2. **I/O 样板逐 scheme 重抄**：containment 校验、有界清单、
   content-type 推断、父目录创建散在各 `Vfs` 实现里，靠共享 helper
   约定而非结构保证，新增协议时容易漏配。
3. **隐式内容变换**：`skill://<name>` 读的是 SKILL.md 去 frontmatter
   的正文——变换藏在实现里，无声明式表达，stat 与 read 对同一资源
   报告的尺寸口径也不一致。

「一个 URI 种类对应一个后端实现」的挂载表骨架不变；需要强化的是
**供给语义**：从「各 VFS 自由实现操作集」到「每个 scheme 都是文件
系统中的一个目录」。

## 决策

### 核心不变量

**每个挂载的 scheme 都有一个 backing root 目录；该 scheme 下的每个
URI 一一映射为 root 内（或经显式映射钩子派生）的一个文件系统路径。**
URI 即路径，router 即文件系统。由此：

- read/write/edit/grep/bash/find 对任何已挂载 scheme 无条件可用
  （grep/bash 经 `source_path` 对齐，后者变为强制字段，见下）；
- 「虚拟资源无底层路径」这一错误类别消失——逻辑资源要参与寻址，
  必须先物化为目录（见「物化挂载」扩展点）。

### `Mount` 声明 + `DirMount` 适配器

新模块 `mount.rs`。`Mount` 是一个 scheme 的最小语义面（纯声明与
钩子，无 I/O）；`DirMount<M: Mount>` 把它适配为完整 [`Vfs`]，统一
实现 stat/read/list/write：

```rust
pub trait Mount: Send + Sync {
    fn scheme(&self) -> &'static str;
    fn capabilities(&self) -> VfsCapabilities;
    /// URI → backing 目录内的绝对路径；实现方负责 containment
    /// 与「资源不存在」的引导文案
    fn locate(&self, uri: &InternalUri) -> Result<PathBuf, VfsError>;
    /// 目录的索引文件：read/stat 以索引文件代表该目录，list 仍列条目
    ///（skill:// 根 → SKILL.md）。默认无索引
    fn index(&self, _uri: &InternalUri) -> Option<&'static str> { None }
    /// read 之后的内容/details 变换（默认原样）；
    /// 变换只发生在 read，stat 恒报告背书文件的事实
    fn transform(&self, _uri: &InternalUri, file: VfsFile) -> VfsFile { file }
    /// 目标不存在的错误（默认 "Could not resolve <href>. <io>"）
    fn not_found(&self, uri: &InternalUri, path: &Path, error: &io::Error) -> VfsError;
    /// 是否支持目录清单（默认支持；纯文件型挂载如 nix:// 关闭）
    fn supports_listing(&self) -> bool { true }
    /// 系统提示词用的一行语义描述（必填：模型必须知道已挂载的
    /// prefix；读写性由 router 渲染时按能力位附加）
    fn describe(&self) -> &'static str;
    fn complete(&self, _query: &str) -> Vec<UrlCompletion> { Vec::new() }
}
```

`DirMount` 的统一实现收编了原先散落的公共行为：目录清单渲染
（`render_listing`）、有界条目（`MAX_LISTING_ENTRIES`）、按扩展名
推断 content-type、写入前创建父目录、统一错误文案。内置 VFS 改写为
纯声明（对外类型名不变，`LocalVfs`/`NixVfs`/`SkillVfs` 成为
`DirMount<…Mount>` 的别名）：

| scheme | backing root | locate 映射 | 钩子 |
|---|---|---|---|
| `local://` | workspace 根 | host+path 拼接 + 词法 containment | complete |
| `skill://` | skill 根目录 | host=skill 名（resolver 查表）+ 根内子路径 | index=SKILL.md（仅根）、transform（正文抽取 + `details.source` 标注）、complete |
| `nix://` | `<workspace>/.nomic/` | `shell` → `flake.nix` | not_found（「写入即创建」引导）、supports_listing=false、complete |

### `source_path` 变为强制字段

`VfsMetadata.source_path: Option<PathBuf>` → `PathBuf`。目录化不变量
下每个资源必有背书路径；grep/bash 的「virtual resource」错误分支与
read 的 url 回退 hint 一并删除。

### 语义对齐（有意的行为变化）

以下三处与原实现存在可验证的微小差异，均属「变换口径收敛」，无
工具输出消费方受影响：

1. **stat 不应用内容变换**：`stat(skill://<name>).size` 报告 SKILL.md
   原始字节数（原为去 frontmatter 后的正文长度）。read 的元数据仍
   在 transform 后对齐（`size` = 变换后内容长度），与原 read 一致。
2. **`VfsFile.url` 统一为不含 query 的 href**：query 是选择器参数，
   非资源标识（与原 local/nix 一致；skill 原回显含 query 的
   raw_href，仅影响带 query 读取时的显示串）。
3. **skill 目录索引仅对根生效**：`index` 钩子按 URI 判定
   （`skill://<name>` 根 → SKILL.md），子目录即使含 SKILL.md 也仍是
   普通清单，与原 resolver 语义逐字节一致。

### 系统提示词注入（prefix + 描述）

挂载集对模型必须是可见的：`VfsRouter::prompt_catalog()` 把各挂载的
`describe` 渲染为 `- <scheme>:// — <描述> (<read-only|read-write>)`
目录，bootstrap 注入 `<internal_uris>` 块。工具装配与提示词渲染
共用 `fs::session_router(resolver, root)`，保证两侧挂载集结构一致。
即席/测试 `Vfs`（非目录化）的 `Vfs::describe` 缺省 `None`，不进
提示词。

### 物化挂载（扩展点，后续落地）

逻辑资源（`history://`、`agent://` 等）以 **Materialized mount** 参与
目录化语义：backing root 是会话缓存目录
（`<data_dir>/nomic/vfs-cache/<session-id>/<scheme>/`，刻意在
workspace 之外），内容由 `MountSource` 钩子生成：

```rust
#[async_trait]
pub trait MountSource: Send + Sync {
    /// 读前同步（实现方负责廉价：dirty-stamp / 增量追加）
    async fn sync(&self, dir: &Path) -> Result<(), VfsError>;
    /// 可写物化挂载的写回；缺省 = 结构性只读
    async fn commit(&self, dir: &Path) -> Result<(), VfsError>;
}
```

`artifact://`（ADR-0040 已规划为落盘文件）天然是 Passthrough；
`history://` 经 `HistorySource` 把 SQLite 转录物化为每消息一个
Markdown 文件后，grep/bash 对其直接可用。物化挂载一期只读
（只实现 `sync`），缓存随会话删除清理。

## 不采用的替代方案

- **保留 trait 分派、仅强制 `source_path` 非空**：I/O 样板与
  containment 仍分散在各实现，物化钩子无处安放。
- **rust-vfs / FUSE**：ADR-0042 已论证（同步 trait、无 scheme 路由、
  无模型友好错误通道）；FUSE 是 OS 层深链，层次错误且跨平台负担重。
- **虚拟优先 + 每 scheme 配专用查询工具**：工具面随 scheme 数膨胀，
  与「一个文件系统」目标相悖。

## 非目标

- router 收编统一 fs 操作之外的进一步强化：containment 词法校验
  集中到 router、原子写（temp+rename）、普通路径工具收敛为
  `local://` 语法糖——按需单提。
- 物化挂载与 `history://` / `artifact://` / `agent://` 的实际落地
  （本文仅定义扩展点）。
- 新增 `ls` 工具：目录化语义下 `router.list` 已就位，工具面扩展
  另行决策。
- 尾挂选择器、TUI 补全接线：维持 ADR-0040/0042 的既有语义。

## 后果

- 新增协议的边际成本降为「一个 `Mount` 声明」：locate + 能力位 +
  describe + 按需钩子，无 I/O 代码。
- 系统提示词携带已挂载 prefix 目录（`<internal_uris>`），新增挂载
  自动对模型可见。
- `VfsMetadata.source_path` 为 `PathBuf`（crate 内 breaking）；
  `vfs_guard` 的虚拟资源错误分支删除，grep/bash 对所有已挂载
  scheme 可用。
- 无背书的自定义 `Vfs`（纯内存/纯网络）不再能表达——这是意图所在：
  参与寻址即参与文件系统。
- 对外行为（工具输出、错误文案、安全边界）除「语义对齐」列出的
  三处外逐项保持；`vfs_tools.rs` 集成套件做回归基线。
