# ADR-0042: 内部 URI 的 VFS 化（scheme 即挂载点）

- 状态：草案
- 日期：2026-09-02
- 修订：ADR-0040（内部 URI 路由系统）

## 背景

ADR-0040 引入的 `ProtocolHandler` 契约是「单方法联合体」：
`resolve()` 返回的 `UriResource` 用 `is_directory` 标志区分文件内容与
目录清单文本，只读性由 `immutable()` + `writable()` + `write()` 缺省
报错三处表达。该形态带来三个结构性问题：

1. **元数据必须整读内容才能获得**：bash `cd local://foo`、write 闸的
   immutable 检查、grep 的 `source_path` 对齐，都要把目标完整读进内存，
   只为取路径或标志位。
2. **目录清单是预渲染文本而非类型化条目**：消费方无法程序化遍历
   （find/glob、按类型过滤），清单格式散在各 handler 里重复实现。
3. **能力表达分散**：只读性/可补全性是三个方法的组合约定，而非一处
   结构性声明，新增协议时容易漏配。

「一个 URI 种类对应一个后端实现」的注册表骨架（`UriRouter`）已被证明
是对的；需要升级的是后端契约的语义层次：从「resolve 返回联合体」到
「标准文件系统操作集」。

## 决策

### crate 改名：`nomic-uri` → `nomic-vfs`

抽象重心从「URI 路由」移到「虚拟文件系统」，crate 名随之更正。
URI 解析器（`parse.rs`）与尾挂选择器（`selector.rs`）作为 VFS 的
**寻址层**保留在 crate 内，实现不变。

### 核心契约：`Vfs` trait

一个 URI scheme 对应一个 `Vfs` 具体实现，挂载进 `VfsRouter`
（`mount(scheme → vfs)`，原 `register` 的更名）：

```rust
#[async_trait]
pub trait Vfs: Send + Sync {
    fn scheme(&self) -> &'static str;
    /// 结构性能力声明：writable / immutable / completion
    fn capabilities(&self) -> VfsCapabilities;
    /// 元数据（不读内容）：kind / content_type / source_path / immutable 覆盖 / size
    async fn stat(&self, uri: &InternalUri) -> Result<VfsMetadata, VfsError>;
    /// 内容读取；目录返回渲染清单文档（派生内容）
    async fn read(&self, uri: &InternalUri) -> Result<VfsFile, VfsError>;
    /// 类型化目录条目；缺省报「不支持目录清单」
    async fn list(&self, uri: &InternalUri) -> Result<Vec<VfsEntry>, VfsError>;
    /// 缺省报只读——只读性是结构性的
    async fn write(&self, uri: &InternalUri, content: &str) -> Result<(), VfsError>;
    fn complete(&self, query: &str) -> Vec<UrlCompletion>;
}
```

类型迁移：

| 旧（ADR-0040） | 新 |
|---|---|
| `ProtocolHandler` | `Vfs`（`fs/` 模块：`SkillVfs` / `LocalVfs` / `NixVfs`） |
| `UriRouter` | `VfsRouter`（`mount` / `stat` / `read` / `list` / `write`） |
| `UriResource` | `VfsFile`（内容 + `meta`）+ `VfsMetadata`（stat 结果） |
| `immutable()` + `writable()` + `supports_completion()` | `VfsCapabilities` 单点声明 |
| handler 对目录逐个盖 `immutable = Some(true)` | router 统一规则：**目录清单恒不可变** |

### immutable 盖章规则（router 统一执行）

`meta.immutable = meta.immutable.unwrap_or(caps.immutable || kind == Directory)`。
单资源覆盖（`Some`）优先；否则只读 VFS 全量不可变，可写 VFS 的文件可变、
目录清单（派生内容）恒不可变。与 ADR-0040 下各 handler 的手工盖章结果
逐一等价。

### 消费方对齐

- write/edit 闸（`vfs_guard.rs`，原 `uri_guard.rs`）：`stat` 取代
  `resolve` 做存在性与 immutable 检查，不再整读内容；edit 的读-改-写
  在闸后显式 `read`。
- grep/bash 的 `source_path` 对齐：`stat` 取代 `resolve`，纯元数据访问。
- read 工具：`router.read` 取代 `router.resolve`，目录清单文本的渲染
  由 `render_listing`（共享 helper，目录在前带 `/`、按名排序）在各
  VFS 的 `read` 内完成，输出契约不变。

### 不采用的替代方案

- **OS 级 URI 协议注册**（`x-scheme-handler` 等）：跨进程深链入口，
  层次错误；本系统是 agent 工具循环内的进程内资源寻址。
- **`vfs` crate（rust-vfs）**：无 scheme 路由层（注册表仍需自研），
  trait 面是同步完整文件系统语义（seek/append/copy/move），对高度虚拟
  的 scheme（skill 解析、nix 单文件视图）大部分操作只能留空，且缺少
  immutable 盖章与模型友好错误通道；引入为净增复杂度。
- **WHATWG 标准 URL 解析**：会把 `skill://plugin:name` 的冒号当端口、
  归一化掉 `..`（containment 校验必须基于原始形态）——ADR-0040 已论证。

## 非目标

- 新协议（`artifact://` / `history://` / `agent://` 等）：仍是
  ADR-0040 规划的后续工作，届时各实现一个 `Vfs` 挂载即可。
- 新 VFS 操作（mkdir/remove/rename/glob）：操作集按当前消费方需求
  冻结为 stat/read/list/write/complete，扩展按需单提 ADR。
- 文件路径的尾挂选择器、TUI 补全接线：维持 ADR-0040 的非目标。

## 后果

- `nomic-tools` 工具构造器更名 `with_uri_router` → `with_vfs_router`；
  `uri_guard.rs` → `vfs_guard.rs`（`guard_writable` / `vfs_source_path`）。
- bash/grep 的 URI 对齐与 write/edit 闸不再触发整读；目录清单格式
  收敛为单一渲染实现。
- 对外行为（工具输出、错误文案、安全边界）逐项保持；ADR-0040 的
  解析器、选择器、安全边界（fail-closed）与协议后端映射不变。
