# ADR-0036: `mem_*` 原生记忆（只读 agent + curator 维护者）

- 状态：草案
- 日期：2026-08-26

## 背景

llm-wiki 模式（区别于 RAG 的"持久复利知识库"）若靠 agent 本体用通用
工具维护，存在结构性错配：edit 契约对跨页修订过脆、手写 index/log 会
漂移、lint 靠 grep 不可靠、无结构化检索。且记忆系统的输入不止外部
文件——**agent 在工作中学习到的内容**（用户纠正、非显而易见的事实、
有权衡的决策、bug 根因）没有文件载体，会随会话历史消失。

经设计讨论确认的顶层决策：

1. **职责分离**：agent 本体只负责**查询**，不承担任何知识库维护；
   维护由独立角色 **curator** 承担；
2. **全局单一知识库**：`config.toml` 配置固定路径，跨 project 共享；
3. **文件系统存储**：markdown 文件为 source of truth（Obsidian 可直接
   打开、git 版本历史免费）；因写入者唯一（curator），双向同步与并发
   写冲突问题不成立；
4. **`mem://` URI 统一寻址**：读什么由 URI 决定，页面与虚拟资源同构。

## 决策

### 职责分离架构

```
┌─ agent 本体(所有会话)───────────────────────────┐
│  mem_read      查询 wiki(页面 + 虚拟资源)        │
│  mem_search    FTS 检索                          │
│  mem_note      fire-and-forget, 扔一条进 notes/  │
│  read mem://   只读浏览(与读文件体验统一)        │
└──────────────────────────────────────────────────┘
              ▲ 读                  │ notes/ 追加(单文件一条, 原子创建)
┌─ curator(唯一写入者, 独立会话)───────────────────┐
│  mem_read + mem_search + mem_write + read/bash   │
│  职责: 摄入外部文件、整合 notes/、蒸馏会话记录    │
│        产出 learning、维护链接、lint、提交版本    │
└──────────────────────────────────────────────────┘
```

**agent 本体零维护负担**：不更新 index、不维护链接、不处理版本冲突。
`mem_note` 是唯一的写通道，语义为"这条值得记住，你看着办"——写入
`notes/` 即返回成功，整合是 curator 的事。

**curator 是一次性 agent 会话**（复用现有 agent 运行时与会话持久化），
带专用系统提示词与完整 mem 工具面，由三个入口手动触发：

- CLI:`nomic mem curate`（可带聚焦指令，如 `nomic mem curate "处理
  raw/inbox 里的三篇论文"`）；
- TUI:`/curate` 斜杠命令（后台会话，不阻塞当前对话）；
- Web:UI 按钮（经 ADR-0030 的 web 会话机制）。

触发时 curator 读取：`notes/` 未整合条目、`raw/inbox/` 新文件、近期
会话记录（ADR-0023 recorder 已落盘），自主决定整合动作；完成后汇报：
整合了哪些条目、新建/更新了哪些页面、lint 发现了什么；运行结束自动
提交版本（见"版本历史"）。

### 工具面

agent 本体（所有会话始终挂载，惰性激活）：

```
mem_read   { uri }                  # 页面与虚拟资源, 返回 { content, rev }
mem_search { query, kind?, limit? } # 检索, 返回 [{ uri, title, snippet }]
mem_note   { content }              # 写入 notes/, 立即返回
```

`mem_search` 独立成工具而非折叠进 URI 空间：检索是只读 agent 最高频的
动作，专用工具给模型带类型的结果契约（uri 可直接喂回 `mem_read`），
比解析虚拟资源的渲染文本更可靠。

curator 会话（经 ADR-0032 recipe 的 `MemoryPolicy` 分化：`ReadOnly` /
`Curator`，这是 recipe 的新入口差异点）额外持有：

```
mem_write { uri, content, mode?, section?, kind?, reason?, source? }
                           # mode: replace(默认)|append; source: 摄入
                           # 外部文件时归档 raw/ 并记 provenance
```

能力折叠对照：目录/状态 = `mem://index`，链接 = `mem://links/{path}`，
健康报告 = `mem://lint`，日志 = `mem://log`。导出为 CLI 子命令
`nomic mem export <dest>`（逃生舱；FS 存储下等价于拷贝 pages/，主要
价值是导出为扁平结构或打包）。

### 存储：markdown 文件树

```
{wiki_root}/                  # config.toml [memory] wiki_root
├── pages/                    # wiki 页面(source of truth), 子目录即层级
│   ├── entities/alice.md
│   └── learnings/nomic-mutation-queue.md
├── raw/                      # 源材料(curator 摄入外部文件时涉及)
│   ├── inbox/
│   └── {category}/
├── notes/                    # mem_note 落点: 一文件一条 {ts}-{id}.md
├── log.md                    # append-only 日志(grep 友好, 前缀 ## [date])
└── .git                      # 版本历史(curator 自动提交)
```

页面为 markdown + YAML frontmatter：

```markdown
---
title: Alice
kind: entity            # source | learning | entity | concept | synthesis
created: 2026-08-26
updated: 2026-08-26
source: raw/papers/attention.pdf   # 仅 kind=source
---

正文, 跨页引用写作 [[entities/bob]]。
```

设计要点：

- **Obsidian 兼容免费回来**：`wiki_root` 直接作为 vault 打开，graph
  view、Dataview、Web Clipper 生态可用；用户可手改，curator 每次运行
  读的是文件当前状态，无索引漂移问题。
- **index/lint/links 仍是渲染视图而非存储文件**：`mem://index` 等由
  `nomic-mem` 实时扫描 pages/ 生成——保留"索引永不漂移"的性质，
  磁盘上不存在会过期的 index.md。log.md 是唯一持久日志（append 成本
  极低且 grep 友好）。
- **版本历史免费回来**：wiki_root 初始化为 git 仓库（jj 可直接接管，
  git-compatible）,curator 每次运行结束自动提交（信息为本次 curate
  摘要），审查/回滚/diff 全部免费。`page_revisions` 之类的自建机制
  不需要了。

### `mem://` URI 语法

```
mem://entities/alice            # 页面 → pages/entities/alice.md
mem://entities/alice.md         # 兼容形式, 归一化时去掉 .md
mem://index                     # 虚拟资源: 页面目录 + 统计 + 状态
mem://links/entities/alice      #   (实时扫描渲染, 不落盘)
mem://lint
mem://log                       #   → log.md 尾部
```

解析规则：去前缀 → 小写归一化 → 去 `.md` 后缀 → 字符集校验（小写
字母、数字、`-`、`_`、`/`，禁 `..` 与空段）→ 页面映射
`pages/{path}.md`，或路由虚拟资源。映射后**断言 resolved 路径仍在
`pages/` 内**（防编码层面的归一化疏漏）。

### 检索：词项扫描（v1）

`mem_search` v1 为零新依赖的词项匹配：遍历 pages/ 下 markdown，按
标题命中 > frontmatter 命中 > 正文命中的权重打分排序，返回
`[{ uri, title, snippet }]`。个人 KB 规模（数百页）下毫秒级。v2 可
加派生索引（tantivy 或 SQLite FTS 缓存，可重建），工具契约不变。

### 并发设计

写入者唯一化后，并发问题大幅收敛：

- agent 本体只读 + `mem_note` 追加：读无锁；note 一文件一条，
  `O_CREAT|O_EXCL` 原子创建，天然无竞争；
- curator 单会话写入：页面写走 tmp+rename 原子替换；log.md 用
  `O_APPEND`；进程内 per-path 互斥锁（复用 `mutation_queue` 模式）
  防同会话内并行工具调用撞同一页；
- 双 curator 并发：靠 git 提交兜底——第二个 curator 提交时若项目
  已被改动，报错并提示重跑（curate 是幂等倾向的：重新扫描 notes/ 与
  raw/inbox/ 即可）。不引入分布式锁。

### `read` 工具的 `mem://` 集成

`ReadTool` 按 ADR-0003 的 resolver 模式增加 mem 分支：
`read("mem://entities/alice")` 浏览页面，只读。`mem_read` 保留（返回
结构化虚拟资源与页面元数据）。分工：`read mem://` 随手浏览，
`mem_read` 正式查询。

### 配置与挂载

```toml
[memory]
wiki_root = "~/kb"
```

未配置时 `mem_read("mem://index")` 返回未配置提示；配置存在则首次访问
自动初始化目录骨架与 git 仓库，零仪式。`MemStore` 不绑 `BaseDir`
（记忆全局）。

### 架构落点

- **新 crate `crates/app/nomic-mem`**：纯逻辑，不依赖 nomic-core；
  零重依赖（frontmatter 解析复用 `nomic-skills` 的保守 YAML 子集
  思路；git 操作用 std::process 调 git CLI，不引 git2）。
  - `uri.rs`：`MemUri` 解析/归一化/校验、虚拟资源路由（纯函数）
  - `store.rs`：目录骨架、`wiki_root` 定位、原子写、per-path 锁
  - `page.rs`：frontmatter 解析、wikilink 提取、页面读写
  - `render.rs`：虚拟资源渲染（index/links/lint/log）
  - `search.rs`：词项扫描与打分
  - `notes.rs` / `raw.rs`：note 追加与源文件归档
  - `vcs.rs`：git init/commit 封装
- **工具薄壳 `nomic-tools/src/mem/`**：`mem_read`/`mem_search`/
  `mem_note`（本体）与 `mem_write`（curator）四个 `AgentTool`；
  `nomic-tools` 依赖 `nomic-mem`。
- **recipe 分化**：`RecipeOpts` 新增 `memory: MemoryPolicy`
  （`ReadOnly` 默认 / `Curator`）。
- **curate 入口**：`nomic-cli` 的 `mem` 子命令（curate/export）、TUI
  `/curate`、web 按钮，共用"构建 curator 会话"逻辑（curator 系统
  提示词 + `MemoryPolicy::Curator` + 上下文收集：notes/ 待整合数、
  raw/inbox/ 新文件数、近期会话清单）。
- **skill 改写**：`llm-wiki` 收敛为两份提示词——本体的"查询纪律"
  （先 search/index 再 read、何时 mem_note）与 curator 的"维护手册"
  （整合流程、kind 分类、wikilink 惯例、lint 修复策略）。

### 实施切片（每片独立过 `check`，可独立 revert）

1. 本 ADR。
2. `nomic-mem`：uri.rs + store.rs（原子写/锁/骨架初始化）+ page.rs
   + notes.rs，纯单测（含路径穿越与并发追加测试）。
3. `nomic-tools`：四个工具 + 虚拟资源渲染 + 词项 search +
   `read mem://` 集成 + recipe `MemoryPolicy` 分化。
4. config.toml `[memory]` + vcs.rs（git init/自动提交）+
   `nomic mem curate/export` CLI + TUI `/curate` + web 按钮 +
   两份 skill + README/CHANGELOG。

## 后果

正面：

- agent 本体工具面仅 3 个，心智模型一句话："记忆是 mem:// 树，查用
  mem_search/mem_read，记用 mem_note"；
- Obsidian 生态与 git 版本历史免费回来（写入者唯一使双向冲突不成立）；
- 零重依赖：无 sqlx/tantivy/git2,`cargo deny/audit/machete` 无压力；
- index/links/lint 为实时渲染视图，永不漂移；
- curator 作为普通 agent 会话实现，复用全部既有基础设施；
- learning 双通道（mem_note 直投 + curator 蒸馏会话记录）覆盖
  "当下想到"与"事后提炼"两种时机。

代价与缓解：

- **多页写入无跨页事务**：单次 curate 中断可能留下半成品；缓解是
  git 自动提交给出清晰回滚点，且 curator 幂等倾向（重跑重新扫描
  待处理项即可续做）。
- **检索为词项扫描**：数百页规模足够；规模上量后加派生索引（v2），
  工具契约不变。
- **learning 的时效性依赖 curator 触发频率**：手动触发意味着记忆
  可能滞后；缓解是 curator 汇报让用户看到积压，未来可加会话结束
  自动钩子（增量，非返工）。
- **curator 质量依赖提示词**：维护手册需在使用中迭代，与 llm-wiki
  原文"schema 共演化"一致——手册放 skill，可随经验更新。
