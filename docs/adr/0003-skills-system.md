# ADR-0003: skills 系统与 `read` 的 `skill://` 读取

## Status

Accepted

## Date

2026-07-27

## Context

ADR-0001 将 skills / prompt templates 规划为声明式定制能力。此前 nomic 只有
read/write/edit/bash 四个工具，`read` 也只读取本地 UTF-8 文本文件，系统中不存在
skill 的目录约定、元数据模型、发现机制或激活语义。

需求要求 `read` 能够支持 skill 读取，并进一步确认目标是完整 skills 系统，而不是一个
临时的 `SKILL.md` 路径特例。设计需要满足：

- skill 是项目可携带、用户可复用的声明式 Markdown 指令；
- 不把 skill 正文全部无条件注入系统提示词，避免上下文膨胀；
- 能显式激活选定 skill，同时允许模型按需读取；
- 保持工具输出契约一致：offset/limit、2000 行 / 50 KiB 截断、翻页提示；
- skill 内容对 agent 是只读引用，不把 `skill://` 伪装成可写文件路径；
- 允许项目级覆盖用户级，避免同名全局 skill 意外影响项目。

## Decision

### 新 crate：`nomic-skills`

新增 `crates/nomic-skills`，集中承载 skill 领域模型与发现逻辑：

- `SkillResolver`：目录发现、覆盖、catalog、resolve、activate、prompt 清单渲染
- `Skill`：名称、`SKILL.md` 路径、skill 根目录、来源层级、解析后的文档
- `SkillDocument`：`description`、`triggers` 和去除 frontmatter 的正文
- `SkillScope`：`project` / `nomic-user` / `agent-user`
- `SkillsError`：非法名称、未找到、frontmatter 错误、扫描/读取错误

依赖方向仍保持 CLI 组装：`nomic-cli → nomic-tools → nomic-core → nomic-ai`，
`nomic-tools` 与 `nomic-cli` 都依赖 `nomic-skills`。

### 目录约定与覆盖规则

一个 skill 是包含 `SKILL.md` 的目录。支持三类位置：

1. 项目级：从 cwd 向上发现 `.nomic/skills` 与 `.agents/skills`
2. nomic 用户级：`$XDG_CONFIG_HOME/nomic/skills` 与 `~/.config/nomic/skills`
3. 通用 agent 用户级：`~/.agents/skills`

同名覆盖规则：

```text
项目级 > nomic 用户级 > 通用 agent 用户级
```

同级项目目录中，越靠近 cwd 的项目目录优先；同一目录中 `.nomic/skills`
优先于 `.agents/skills`。

skill 名限制为 1～64 个小写 ASCII 字母、数字、`-`、`_`，且不能以 `-`/`_`
开头或结尾。该限制避免路径穿越、URI 歧义和大小写跨平台问题。

### `SKILL.md` 元数据

支持可选 frontmatter：

```markdown
---
description: Review Rust changes
triggers: [rust, review]
---

# Review steps
...
```

当前实现使用一个保守的 YAML 子集解析器：

- `description: text`
- `triggers: [a, b]` 或 `- item` 多行列表
- 简单未知标量字段忽略
- 复杂 YAML 结构明确报错

`description` 缺省时回退到正文第一个非空行。正文用于激活和 `skill://` 读取，
frontmatter 不作为正文重复返回。

### 系统提示词集成

bootstrap 从 cwd 构造一个 `SkillResolver`：

- 将所有可用 skill 的名称、描述和 triggers 以简短清单注入 system prompt；
- 清单明确指示模型通过 `read` 读取 `skill://<name>` 后再遵循该 skill；
- CLI `--skill <NAME>`（可重复）显式激活 skill，完整正文注入
  `<active_skill ...>` 块；
- 未显式激活的 skill 只有元数据清单，不注入完整正文。

这遵循渐进式披露：目录信息常驻，正文按需读取或显式激活。

`skill://` scheme 与 `<active_skill ...>` 标签是跨模块契约，其唯一定义在
`nomic-skills` 接口中（`SKILL_SCHEME` 常量、`ActivatedSkill::prompt_tag` /
`parse_active_skill_tag`）；`read` 工具、bootstrap 与 TUI 均调用这些定义，
不在消费方各自硬编码。

### `read` 的 `skill://` 语义

`ReadTool` 可注入 `SkillResolver`。当参数为：

```json
{"path": "skill://rust-review"}
```

时：

1. 校验并解析 skill 名；
2. 定位覆盖规则生效后的 `SKILL.md`；
3. 读取去除 frontmatter 的正文；
4. 复用现有 `offset/limit`、2000 行 / 50 KiB 头部截断和翻页提示；
5. `details.source` 标注：

```json
{
  "source": {
    "kind": "skill",
    "uri": "skill://rust-review",
    "name": "rust-review",
    "scope": "project",
    "path": "/repo/.nomic/skills/rust-review/SKILL.md"
  }
}
```

`skill://` 是只读资源协议。`write` 与 `edit` 不接受该协议；如需修改 skill，
用户必须明确要求修改其 backing file。

## Alternatives Considered

### 只做 `skill://` 只读支持

范围最小，但无法回答 skill 如何发现、如何展示、如何激活，也无法构成完整 skills
系统。Rejected。

### 自动把所有 skill 正文注入系统提示词

简单但上下文成本不可控，项目目录和用户目录增长后会污染所有任务。Rejected；采用
清单常驻 + 正文按需/显式激活。

### 引入完整 YAML parser

frontmatter 初始只需要 description/triggers。完整 YAML parser 会增加依赖与错误
语义，而当前子集可保持行为简单、错误明确。后续需要复杂元数据时再升级为完整
YAML 解析并记录 ADR 修订。

### 把 skill 解析逻辑放入 `read.rs`

会让文本读取工具承担目录发现、覆盖和元数据模型职责，后续添加 URL/artifact
等资源时会变成巨型分派器。Rejected；领域逻辑独立到 `nomic-skills`。

## Consequences

- `read` 从“仅本地文件”扩展为“本地文件 + 只读 skill 资源”。
- `ReadTool` 不再是零大小类型，CLI 使用 `default_tools_with_skills()` 注入
  `SkillResolver`；不带 resolver 的 `ReadTool::new()` 仍可用于测试和基础文件读取。
- skills 已具备发现、frontmatter、prompt 清单、显式激活和 `skill://` 分页读取；
  尚不包含运行时动态激活命令、session 中持久化激活状态、token 预算或子 skill
  资源引用。
- 后续若要支持 `artifact://`、URL、压缩包等资源，应抽象统一资源路由，而不是继续
  把新类型直接堆进 `read.rs`。

## 修订（2026-08-13）：子资源寻址、可见性元数据与诊断

对照 omp 的逆向结论补齐已验证有价值的差距，不改变本 ADR 的核心决策
（skill 只读、三层 scope 覆盖、清单常驻 + 正文按需读取）：

### `skill://<name>/<path>` 子资源

`skill://` URI 在首个 `/` 切分名称与子路径：无子路径（含 `skill://name/` 与
`skill://name/.`）仍返回去除 frontmatter 的正文；子路径返回 skill 根目录内的
文件内容（复用 offset/limit、2000 行 / 50 KiB 截断契约）或目录清单（一行一条目，
目录以 `/` 结尾）。`details.source` 增加 `resource` 字段。

穿越防护只做词法规范化 + 深度计数（`SkillResolver::resolve_resource` →
`resolve_resource_path`）：拒绝绝对路径、拒绝 `..` 越出 skill 根，不做符号链接
解析——skill 目录内指向外部的符号链接不在此拦截（skill 对用户是可信资产，
与“skill 正文可任意引用绝对路径”的事实一致）。

### frontmatter `enabled` / `hide`

手写 YAML 子集新增两个布尔标量（非布尔值报 `InvalidFrontmatter`，仍不升级完整
YAML 解析器）：

- `enabled: false`：catalog 直接跳过（`resolve` 也找不到），用于显式关闭；
- `hide: true`：可 `resolve` / 显式激活，但不出现在 prompt 清单
  （对齐 omp 的 `hide` / `disable-model-invocation`，用于只供显式调用的 skill）。

### 注入指引与诊断

- `<active_skill>` 注入块尾追加 `[Skill directory: <root 绝对路径>]` 指引：
  正文中引用的相对路径以 skill 根目录为基准解析，附属文件经
  `skill://<name>/<path>` 或文件系统读取、脚本经 bash 执行（对齐 omp 的
  baseDir 注入）；`ActivatedSkill` 相应增加 `root` 字段。
- prompt 清单头部说明子资源读取方式；bootstrap 启动时把
  `catalog_with_diagnostics` 的加载错误以 stderr 告警 + tracing 日志输出
  （此前坏 skill 静默跳过，无从排查）。
- TUI `/skill:<name>` 后首个空白起的自由文本作为附加上下文，注入消息尾部以
  `User: <args>` 追加（参考 omp 的 user-invocation 模板）。

### 刻意排除（维持原决策）

| omp 特性 | 排除理由 |
| --- | --- |
| 可写 skill / auto-learn | 违背“skill 对 agent 只读”决策；自修改 prompt 资产风险高于收益 |
| 多 provider 生态发现（codex/opencode/插件包） | 复杂度与维护面大，无需求证据 |
| frontmatter `name` 覆盖目录名 | 名称 = 目录名更直观，避免 URI 与磁盘结构脱节 |
| `skill://<name>` 返回含 frontmatter 的完整文件 | 正文 / 元数据分离的设计更干净，保留 |
