# ADR-0045: 统一 entry payload 模型（角色 + Part 内容块）

## Status

Accepted

## Date

2026-09-03

## Context

会话历史在存储层（`entries.payload`）长期是两种格式的混合体：消息条目存
`Message` 的 serde JSON（user / assistant / tool_result 三角色各自独立结构体），
压缩条目（`kind = 'compaction'`）存独立的 `CompactionRecord` JSON。问题：

- compaction 不是一等角色：payload 类型旁路于消息模型，写入/重放/预览
  每条路径都要按 `kind` 列分叉处理；
- 内容块词汇表分裂为 `UserContent`（text/image）与 `AssistantContent`
  （text/thinking/tool_call）两个 enum，二者实为同一词汇表按角色的切片；
- `kind` 列与 `role` 列信息冗余（`kind='compaction'` 恒等价于
  `role='compaction'`）。

目标（需求已确认）：entry 增加 compaction 角色用于存储压缩内容；entry 由
若干 part 组成（text / image / tool_call 等），统一为一种序列化形状。

## Decision

### 统一 Entry 模型（`nomic_ai::entry`）

```rust
pub struct Entry {
    pub role: EntryRole,        // user / assistant / tool_result / compaction
    pub parts: Vec<Part>,       // 有序内容块
    pub response: Option<ResponseMeta>, // 仅 assistant：api/model/usage/stop_reason/…
    pub timestamp: u64,
}

pub enum Part {
    Text(TextContent),
    Thinking(ThinkingContent),
    Image(ImageContent),
    ToolCall(ToolCall),
    ToolResult(ToolResultPart),   // tool_call_id/tool_name/is_error/details/content
    Compaction(CompactionRecord), // summary/kept_count/tokens_before（自 nomic-session 上移）
}
```

- **compaction 成为一等角色**：压缩内容存于 `parts: [Compaction(..)]`，不再
  旁路；`entries.kind` 列随之删除（role 完整表达条目种类）。
- **part 词汇表统一**：`UserContent` / `AssistantContent` 不再承担存储格式，
  各角色允许的 part 种类由转换规则约定（构造侧保证，反序列化宽容以便前向
  兼容）：user → Text/Image；assistant → Text/Thinking/ToolCall；
  tool_result / compaction → 恰好一个对应块。
- **assistant 响应元数据挂在条目上**（`response` 字段）：usage/stop_reason
  等是一次响应一份的属性，不属于任何单个 part。

### 只改存储层，内存模型不动

agent 持有的历史仍是 `Vec<Message>`，provider wire 转换、agent loop、
TUI/web 渲染全部零改动。`Entry` 与 `Message` 的结构同构由
`nomic_ai::entry` 中的双向转换维系（`Entry::from(&Message)` /
`Entry::to_message()`），是全库唯一的格式定义点。`nomic-session` 的公开
API（`append_message` / `append_compaction` / `load_messages` / …）签名
不变，payload 在 crate 边界内部转换。

### 重放语义不变

compaction 的 `kept_count` 相对计数语义（ADR-0005）原样保留：重放遇到
compaction 条目仍经 `apply_compaction` 截尾 + 前置合成摘要消息。曾评估过
「位置语义」（丢弃 compaction 之前的所有条目），但 kept tail 不落库的现状
要求每次压缩重复落库保留消息（DB 膨胀、`/tree` 出现重复节点、存量迁移
在分支场景下无法可靠重挂父指针），代价远超收益，放弃。

### 存量数据一次性迁移

- SQL 迁移 0012 删除 `entries.kind` 列；
- payload 重写（旧 `Message` / `CompactionRecord` JSON → `Entry` JSON）由
  `nomic_session::migration` 在 sqlx 迁移后于 Rust 侧执行（格式定义依赖
  serde 类型，无法用纯 SQL 表达），`PRAGMA user_version`（0 → 1）把关，
  结构性判别（含 `parts` 即新格式）保证幂等；解析失败的行保留原样并告警
  （加载行为与迁移前一致）；
- 迁移后加载路径只认 Entry 格式，不保留双格式兼容代码。

## Consequences

- 新增内容块种类（如未来的引用/附件块）只需扩展 `Part` enum 与各角色的
  转换规则，存储形状不变；
- `entries` 表少一列，所有「消息条目」过滤条件统一为 `role <> 'compaction'`
  / `role = 'user'`；
- 旧版本应用打开迁移后的库将无法解析 payload（视为损坏数据）：存储格式
  前向不兼容，属可接受的升级路径（桌面单用户场景，版本随应用升级）。
