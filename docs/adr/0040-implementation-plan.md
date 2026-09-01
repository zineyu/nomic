# 内部 URI 路由系统实施计划（ADR-0040）

目标：把 oh-my-pi 内部 URL 架构移植到 nomic，落地 6 个有真实后端的协议。
技术栈：Rust 2024、tokio、nextest；全部经 `devenv shell` 的 `check` 验证。

## 文件地图

| 路径 | 动作 | 内容 |
|---|---|---|
| `crates/app/nomic-uri/` | 新建 crate | 见 ADR-0040 目录结构 |
| `Cargo.toml`（workspace） | 改 | members += `crates/app/nomic-uri` |
| `crates/app/nomic-tools/Cargo.toml` | 改 | 依赖 += nomic-uri |
| `crates/app/nomic-tools/src/read.rs` | 改 | URI 分支走 router；local:// 提升；conflict:// 短路 |
| `crates/app/nomic-tools/src/write.rs` | 改 | conflict:// 短路；router 可写 scheme；未知 URI 报错 |
| `crates/app/nomic-tools/src/grep.rs` | 改 | URI → source_path 交 ripgrep |
| `crates/app/nomic-tools/src/bash.rs` | 改 | 命令内 URI 展开 |
| `crates/app/nomic-tools/src/lib.rs` | 改 | `default_tools*` 接收 `Arc<UriRouter>` |
| `crates/app/nomic-tools/src/multi_agent.rs` | 改 | wait_result/wait_all 落盘 AgentOutputStore |
| `crates/app/nomic-tools/src/truncate.rs` | 改 | 截断落 artifact，Continuation 携带 artifact id |
| `crates/runtime/nomic-core/src/compaction/mod.rs` | 改 | `extract_file_ops` 跳过 `scheme://` 路径 |
| `crates/app/nomic-cli/src/bootstrap.rs` | 改 | 系统提示词更新；组装 router |
| `crates/app/nomic-cli/src/agent_recipe.rs` | 改 | skill_resolver 注入点改为 router 注入 |

## 任务拆分（每个任务 = 一个原子 commit，先红后绿）

### T1: nomic-uri 骨架 + 解析器

- 新 crate；`parse.rs` 实现 `extract_uri_scheme` / `parse_internal_uri`。
- 测试先行：`crates/app/nomic-uri/src/parse.rs` 内 `#[cfg(test)]`——
  - `skill://plugin:name` host 含冒号不炸；
  - `C:\foo`、`foo.ts:50`、`Makefile:12` 不被当 URI；
  - `raw_host` 保大小写、`raw_path` 保 `..`。
- 命令：`cargo nextest run -p nomic-uri`（先红：crate 不存在 → 写实现 → 绿）。

### T2: ProtocolHandler trait + UriRouter

- `handler.rs` + `router.rs`：register/resolve/write/complete；immutable 盖章；
  未知 scheme 错误列举可用协议；无 write 的 scheme 报 read-only。
- 测试：假 handler 注册两个 scheme，断言盖章、read-only 报错、未知 scheme 报错文本。

### T3: URI 选择器

- `selector.rs`：`split_uri_selector`（白名单 scheme 激进剥离）+ `parse_selector`
  （`:N` `:A-B` `:A+C` `:raw` `:range:raw` 复合；畸形抛错）。
- 测试：与 oh-my-pi `read-selector.ts` 对齐的用例表，含 `artifact://5:conflicts:1-1` 抛错。

### T4: skill:// 迁移

- `handlers/skill.rs`：把 `ReadTool::execute_skill_read` 的解析逻辑搬入
  `SkillProtocolHandler::resolve`（内部仍调 `SkillResolver::resolve_resource`），
  目录清单、正文、文件三种资源形态映射为 `UriResource`；immutable = true。
- read.rs 的 `skill://` 特判删除，改走 router。
- 测试：现有 `nomic-tools/tests/tools.rs` 的 skill:// 用例（L128-279）必须
  原样全绿——这就是迁移的回归网。

### T5: ArtifactManager + artifact://

- `handlers/artifact.rs`：`data_dir/nomic/artifacts/<session_id>/` 惰性创建；
  ID = 扫描 max+1（共享初始化 future 防并发重种子）；`resolve` 有 8 MiB 上限，
  超限错误信息指引 `:1-3000` 选择器。
- `truncate.rs`：新增 `Continuation::Artifact { id, path }`；read/bash 截断时
  落盘并在 notice 附 `read artifact://<id>` 指引。
- 测试：分配两个 ID 连续；重开目录续号不覆盖；超限报错文本含选择器指引。

### T6: local:// 读写

- `handlers/local.rs`：根 = `<artifacts_dir>/local/`；三层 containment
  （词法 → 前缀 → canonicalize）；immutable = false；`write` 实现；
  目录清单资源。
- write.rs 接入 router；未知 URI 形写目标报错。
- 测试：`local://../escape` 拒绝；符号链接逃逸拒绝；write→read 往返一致。

### T7: read 工具全面接入 + conflict:// 读侧

- read.rs：URI 判定（`router.can_resolve`）→ 选择器剥离 → resolve → 分页；
  `local://` 有 source_path 且为文件时提升回普通文件管线；
  `conflict://<N>[/<side>]` 短路（`nomic-uri/src/conflict.rs` 的
  `ConflictHistory`：扫描 `<<<<<<<`/`=======`/`>>>>>>>`，同位置复用 id）。
- 测试：artifact 分页、local 提升、conflict 注册与 `conflict://1/ours` 渲染。

### T8: conflict:// 写侧

- write.rs：`conflict://<N>` 内容定位 + splice（落盘前重校验 marker 块）；
  `conflict://*` 批量 + per-id 指令（`<id>: @ours` 行，部分畸形即抛错）。
- 测试：单个解决、批量、stale marker 报错、畸形指令块报错。

### T9: agent://

- multi_agent.rs：`AgentOutputStore`（`Arc<RwLock<HashMap<AgentId, String>>>`），
  wait_result/wait_all 成功时写入最终 assistant 文本；store 作为
  `AgentOutputSource` 注入 router 构造。
- `handlers/agent.rs`：`agent://<id>` 渲染该输出；immutable = true。
- 测试：create→wait→read agent:// 往返；未知 id 错误列举可用 id。

### T10: history://

- `handlers/history.rs`：`history://` 列会话（id/标题/时间，来自
  nomic-session 的会话清单 API）；`history://<id>` 只读拉取 entries
  渲染 markdown 转录。
- 测试：内存 SQLite 建两会话，断言清单与转录内容。

### T11: grep/bash/compaction/提示词集成

- grep.rs：URI → `resolve(path_only)` → source_path；无路径目录资源拒绝。
- bash.rs：命令内 `skill://|local://|artifact://` 展开（引号内不展开）。
- compaction/mod.rs：`is_uri_scheme_path` 过滤。
- bootstrap.rs：read 描述更新为内部 URI 列表；组装 router 注入工具集。
- 测试：grep 搜 artifact 内容命中；bash `cat skill://x/SKILL.md` 等价于
  cat 真实路径；compaction FileOps 不含 `artifact://` 条目。

### T12: 收官

- `check` 全绿（fmt/clippy/nextest/doc/deny/audit/machete/taplo/typos/文件行数）。
- README 工具章节补内部 URI 说明；ADR-0040 状态改「已接受」。

## 验证

- 每个任务：`cargo nextest run -p <受影响的 crate>`。
- 收官：`check`（CI 等价）。
- 端到端手验：交互模式里让 agent 读 `skill://`、`history://`，
  触发一次超截断 bash 后经 `artifact://` 恢复。

## 风险

- **T4 回归**：skill:// 行为迁移依赖现有测试网兜底，先跑绿再动手。
- **T7 read 分叉**：read.rs 已有图片/目录/截断分支，URI 分支插入点
  须在文件路径解析之前、且 local:// 提升要与现有 `read_text_path`
  复用，避免双份分页逻辑。
- **T9 生命周期**：agent 关闭（close_agent）后输出是否保留需决策——
  计划默认保留至会话结束（store 不随 close 清除），与 history:// 互补。

---

## 实施进度（2026-08-07 起）

- T1–T3：`nomic-uri` 解析器 / router+trait / 选择器，done。
- T4：`skill://` 迁入 router；`default_tools_with_skills_in_shared` 构建会话级
  `Arc<UriRouter>`，read 经 router 分发；失败矩阵 4.2 全绿。
- T5：write/edit 经 `uri_guard::guard_writable` 闸：只读拒绝、未知 scheme 报
  UnknownScheme、可写协议走 resolve→替换→router.write（测试用内存协议验证）。
- T6：`@skill://` URI mention 形式（补全 + 展开，与旧式 `@skill:` 等价共存）。

### 与计划的偏差记录

1. **仓库现状差异**：nomic 没有 `nomic-tui` / `nomic-commands` / `nomic-artifacts`
   crate——TUI 在 `nomic-cli/src/tui/`，slash 命令注册表在 `tui/app`（`COMMANDS`），
   artifacts 子系统不存在。移植按现状适配。
2. **T6 的 commands.ts 移植推迟**：oh-my-pi 的 new/copy/export 命令以 artifact
   store 为依托，nomic 尚无 artifact 子系统；且 nomic 已有自己的 slash 命令体系，
   整体另建 `nomic-commands` 属独立架构决策。推迟到 T10（artifact://）一并评估。
3. **T6 的 mention 语法**：nomic 已有 `@skill:`/`@file:` mention 体系（含 chat
   折叠、web 端）。URI 形式 `@skill://` 作为等价形式并入（补全 + 展开），不替换
   既有语法。
