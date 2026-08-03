# ADR-0008: prompt templates

## Status

Accepted

## Date

2026-08-03

## Context

ADR-0001 将 skills / prompt templates 规划为声明式定制能力；skills 已由
ADR-0003 实现，prompt templates 仍缺失。设计参照 pi 的 prompt templates
（<https://pi.dev/docs/latest/prompt-templates>）：

- 模板是 Markdown 片段，输入 `/name` 即展开为完整 prompt 提交；
- 需要位置参数、默认值与切片等轻量占位符语法；
- 与 skills 一样支持项目级 / 用户级目录与同名覆盖；
- 与 nomic 既有约定一致（`.nomic/` 目录、XDG 用户目录、CLI/配置文件显式路径）。

需要决策的点：

- 模板与内建 slash 命令同名时谁优先；
- 展开发生在哪一层（TUI 状态层 / driver / agent core）；
- print 模式是否支持模板调用。

## Decision

### 新 crate：`nomic-prompts`

新增 `crates/nomic-prompts`，集中承载模板领域模型与发现逻辑：

- `PromptResolver`：目录发现、覆盖、catalog、resolve、expand
- `PromptTemplate`：名称、文件路径、来源层级、描述、`argument-hint`、正文，
  `expand(args)` 做参数展开
- `PromptScope`：`user` / `project` / `explicit`
- `expand_template` / `split_arguments` / `expand_invocation`：纯函数，
  展开与参数切分不依赖 IO，便于 TUI 状态层脱离终端单测
- `PromptsError`：非法名称、未找到、frontmatter 错误、扫描/读取错误、引号未闭合

frontmatter 解析复用与 skills 相同的最小 YAML 子集口径（`description` 支持块标量、
`argument-hint` 为简单标量、未知标量键忽略、未知嵌套块跳过、flow 集合报错），
有意不引入完整 YAML 依赖；两个 crate 各自持有一份小解析器，保持零耦合。

### 目录约定与覆盖规则

一个模板是一个 `.md` 文件，文件名（去掉 `.md`）即 `/name` 命令名：

1. 项目级：从 cwd 向上发现 `.nomic/prompts/*.md`（越靠近 cwd 越优先）
2. 用户级：`$XDG_CONFIG_HOME/nomic/prompts/*.md` 与 `~/.config/nomic/prompts/*.md`
3. 显式：配置文件 `prompts` 数组与 `--prompt-template <PATH>` 指定的文件或目录

同名覆盖规则：`显式 > 项目级 > 用户级`。目录发现非递归；`--no-prompt-templates`
关闭目录发现（显式路径仍生效）。模板名规则与 skill 名一致（1～64 个小写 ASCII
字母、数字、`-`、`_`，不以 `-`/`_` 开头或结尾），非法名称跳过并告警。

### 参数占位符语法（与 pi 对齐）

- `$1`、`$2`、...：位置参数（缺失展开为空）
- `$@` / `$ARGUMENTS`：全部参数（空格连接）
- `${1:-default}` / `${@:-default}` / `${ARGUMENTS:-default}`：缺失或为空时用默认值
- `${@:N}`：第 N 个（1 起）及之后的全部参数；`${@:N:L}`：从第 N 个起取 L 个

无法识别的 `$` 序列（`$0`、`$x`、非法 `${...}`）保持字面量。参数串按 shell 风格
切分：单引号字面、双引号内 `\"`/`\\` 转义、引号外 `\` 转义下一字符；引号未闭合
时报错，不发送。

### 调用与展开位置

展开发生在 CLI 层，agent core 无感知（展开结果就是一条普通 user prompt）：

- 交互 TUI：`/name args...`（`/name:args` 亦可）在 `App` 状态层先经内建命令
  解析，未命中再按模板调用展开，经 `Effect::Prompt` 走与普通输入完全相同的路径；
  内建命令优先于同名模板。补全弹层把模板与内建命令一起列出，展示
  `argument-hint` 与描述。
- print 模式：`-p` 的 prompt 以 `/` 开头时按模板调用展开；未知名称硬报错。

## Consequences

- 模板与 skills 共用发现/覆盖/诊断模式，用户认知成本一致；
- 展开是纯函数且发生在状态层，TUI 路径可完整单测；
- 模板不进入系统提示词，不影响上下文 token；
- `argument-hint` 仅用于补全展示，不参与展开；
- 会话内展开后的 prompt 随 session 落库，resume 后看到的是展开结果而非
  `/name` 调用本身（与普通输入行为一致）。
