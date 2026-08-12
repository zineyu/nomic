# nomic

[![CI](https://github.com/zineyu/nomic/actions/workflows/ci.yml/badge.svg)](https://github.com/zineyu/nomic/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://github.com/zineyu/nomic)
[![Rust 1.97+](https://img.shields.io/badge/rust-1.97%2B-orange.svg)](https://github.com/zineyu/nomic)

Rust 编码 agent —— [pi-coding-agent](https://github.com/badlogic/pi-mono) 的 Rust 复刻。
借鉴 pi 的核心思想（统一流式 provider 抽象、事件驱动的 agent loop、与模型的工具输出契约），
但以 Rust 风格实现（serde 即校验、`CancellationToken`、增量式流事件）。设计决策见
[docs/adr/0001](docs/adr/0001-pi-rust-architecture.md)。

## 特性

- **多 provider**：Anthropic Messages 与 OpenAI Completions 兼容端点（DeepSeek、各类网关代理等），
  模型规格分层解析（配置 > [models.dev](https://models.dev) > 中性兜底）
- **双运行模式**：ratatui 全屏交互 TUI（vim-like 模式化交互）+ 非交互 print 模式（管道可用）
- **排队输入**：运行中 `Enter` 把消息排入统一消息队列（当前步骤完成后注入本轮运行，
  未清空则持续续行；运行被取消或失败时队列保留，恢复后按序作为下一轮发送）；
  oil.nvim 式 QUEUE 模式编辑队列（就地编辑/删除/换位），设计见
  [ADR-0014](docs/adr/0014-unified-message-queue.md)
- **八件工具**：`read` / `write` / `edit` / `bash` / `grep` / `find` / `todo_read` / `todo_write`，
  schemars + serde 即校验，parallel 执行
- **持久会话**：SQLite 树形存储，支持 resume（按 cwd 隔离）、会话分支浏览与创建、`sessions list`
- **上下文工程**：AGENTS.md 向上发现注入、skills 系统、prompt templates、自动/手动上下文压缩
- **图片输入**：`--image` 附件、`/image` 暂存、`Ctrl+V` 剪贴板图片粘贴
- **外部编辑器**：INSERT 下 `Ctrl+G` 挂起 TUI，用 `$VISUAL`/`$EDITOR`
  （缺省 `vi`）编辑当前草稿（长文/多行 prompt），保存退出后写回；
  编辑器异常退出或内容为空时原草稿保留，
  设计见 [ADR-0017](docs/adr/0017-external-editor.md)
- **工程化**：devenv 统一开发环境，`check` 一键执行与 CI 等价的全部检查，Nix flake 打包

## 目录

- [安装](#安装)
- [快速上手](#快速上手)
- [使用](#使用)
  - [运行模式](#运行模式)
  - [TUI 键位（vim-like）](#tui-键位vim-like)
  - [TUI slash 命令](#tui-slash-命令)
  - [会话恢复与分支](#会话恢复与分支)
  - [图片输入](#图片输入)
  - [上下文压缩](#上下文压缩)
  - [日志](#日志)
- [配置](#配置)
  - [模型选择（sqlite）](#模型选择sqlite)
  - [配置文件](#配置文件)
  - [多 provider 与模型规格](#多-provider-与模型规格)
- [上下文文件](#上下文文件)
  - [AGENTS.md](#agentsmd)
  - [Skills](#skills)
  - [Prompt Templates](#prompt-templates)
- [开发](#开发)
- [发布](#发布)
- [路线图](#路线图)
- [License](#license)

## 安装

### 预编译二进制

从 [GitHub Releases](https://github.com/zineyu/nomic/releases) 下载对应平台的
`nomic-<版本>-<target>.tar.gz`（提供 x86_64/aarch64 的 Linux 与 macOS 构建，
附 SHA256 校验和），解压后将 `nomic` 放入 `PATH` 即可。

### Nix（flake）

```bash
nix run github:zineyu/nomic              # 直接运行
nix profile install github:zineyu/nomic  # 安装到 profile
nix profile install github:zineyu/nomic/v0.1.0  # 或安装指定版本（tag 即 flake ref）
```

构建工具链由 `rust-toolchain.toml` 固定，与 devenv / CI 完全一致；设计见
[docs/adr/0006](docs/adr/0006-nix-flake-packaging.md)。

### 从源码构建

```bash
cargo install --path crates/nomic-cli  # 或 cargo build --release -p nomic-cli
```

## 快速上手

```bash
# 1. 配置 API key
export ANTHROPIC_API_KEY=sk-ant-...     # 或 OPENAI_API_KEY / OPENAI_BASE_URL

# 2. 首次启动必须显式指定模型（无内置默认 provider / 模型）
nomic --model anthropic/claude-sonnet-4-5

# 3. 之后在 TUI 内经 /models 切换，选择结果跨会话记住
```

## 使用

### 运行模式

```bash
# 交互 TUI（缺省，设计见 docs/adr/0002）
nomic --model anthropic/claude-sonnet-4-5

# print 模式（非交互，流式输出到 stdout，管道可用）
nomic -p "列出当前目录的文件"

# OpenAI 兼容端点（DeepSeek、代理网关等）
nomic -p "..." --provider openai --base-url https://your.gateway/v1 --model deepseek-chat

# 推理模型
nomic -p "..." --reasoning low
# TUI 内：/models 选择模型后可为推理模型选择思考级别（含 off 关闭）
```

### TUI 键位（vim-like）

交互为 vim-like 模式（[ADR-0011](docs/adr/0011-vim-like-interaction.md)），
默认 INSERT（输入），Esc 逐层退回：

| 模式 | 键位 | 说明 |
| ---- | ---- | ---- |
| INSERT | `Enter` / `Tab` | 发送消息（运行中排入队列，当前步骤完成后注入本轮）/ 补全 |
| INSERT | `Esc` | 关弹层 / 进入 NORMAL（取消运行用 `Ctrl+C`） |
| INSERT | `Ctrl+W` `Ctrl+U` `Ctrl+A/E` `Alt+B/F` | 删词 / 清行 / 行首行尾 / 词移动 |
| INSERT | `Ctrl+G` | 外部编辑器（`$VISUAL`/`$EDITOR`，缺省 `vi`）编辑当前草稿；保存退出后写回，异常退出或内容为空时原草稿保留 |
| NORMAL | `j` `k` `Ctrl+D/U` `gg` `G` | 滚动 / 半页 / 顶部 / 底部 |
| NORMAL | `[m` `]m` `[t` `]t` | 跳上/下一条消息、工具调用 |
| NORMAL | `/` `n` `N` | 聊天搜索与跳转 |
| NORMAL | `yy` `yc` `V`+`y` | 复制消息 / 复制代码块 / 选择后复制 |
| NORMAL | `x` `dd` `dw` | 编辑草稿 |
| NORMAL | `Q` | 打开队列编辑（QUEUE 模式，队列非空时） |
| NORMAL | `i` `a` `A` `I` `Enter` | 回到输入 |
| NORMAL | `:` | 命令输入（预填 `/`） |
| QUEUE | `j` `k` `gg` `G` | 移动条目游标 |
| QUEUE | `i` `o` `O` `Enter` | 就地编辑 / 下方新增 / 上方新增（`Enter`/`Esc` 保存，空文本即删条目） |
| QUEUE | `dd` `x` `J` `K` | 删除条目 / 下移 / 上移（换位）；打开期间冻结发送，退出恢复 |
| picker | 输入即过滤 · `↑/↓` 选择 · `Home/End` 首尾 | 适用于 `/resume`、`/models`、`/tree` |
| 通用 | `Ctrl+C` | 取消运行 / 退出 |
| 通用 | `↑/↓` `PgUp/PgDn` 滚轮 | 滚动 |
| 通用 | `Shift`+拖选 | 复制文本（TUI 捕获鼠标用于滚轮，原生选择需按住 Shift） |

### TUI slash 命令

输入 `/` 自动补全；`/help` 查看全部：

| 命令 | 说明 |
| ---- | ---- |
| `/help` | 显示可用命令 |
| `/new` | 清空上下文，开启新对话（新 session） |
| `/resume` | 交互选择并恢复历史 session（切换上下文与落库目标） |
| `/tree` | 浏览会话树，选择非工具调用条目作为新分支起点（原分支保留） |
| `/models` | 跨 provider 切换模型（`/models:<provider>/<模型id>` 亦可）；推理模型联动选择思考级别 |
| `/skill` `/skill:<name>` | 列出可用 skill / 手动载入指定 skill |
| `/image:<路径>` | 为下一条消息附加图片（可多次附加） |
| `/compact [聚焦指令]` | 手动压缩上下文 |
| `/retry` | 重试最近一轮失败的响应 |
| `/copy` | 复制最新一条消息到剪贴板 |
| `/thinking` | 切换 thinking 内容折叠/展开显示 |
| `/goal` | 开关 goal 模式：开启后 react loop 停止时若 todo 未全部完成，自动以 user 消息追问 |
| `/quit`（`/exit`） | 退出 TUI |

运行中本地 slash 命令（`/help`、`/copy` 等）照常可用，不被工具调用阻塞。
运行中输入的普通消息与模板调用按 `Enter` 排入统一消息队列（见上「排队输入」）；
会话命令（`/compact`、`/retry`、`/models` 等）仍须等本轮结束。

### 会话恢复与分支

```bash
nomic --continue        # 当前目录下最近的 session（按 cwd 隔离）
nomic --session <ID>    # 指定 session（可跨目录，会有提示）
nomic resume            # 交互选择器（↑/↓ 或 j/k 移动，Enter 确认，Esc/q 取消）

# 查看历史 session（标题、最后更新时间、消息数、目录）
nomic sessions list
```

- TUI 内随时可用 `/resume` 打开同一选择器：选中后替换当前上下文并切换落库目标。
- 会话分支：`/tree` 浏览当前 session 的消息树，选择非工具调用条目作为新分支起点——
  上下文回到该条目，后续对话写入新分支，原分支保留可回访。

### 图片输入

print 模式用 `--image` 附带图片（可重复；png/jpeg/gif/webp）：

```bash
nomic -p "这张截图里有什么错误" --image screenshot.png
```

交互 TUI 用 `/image` 为下一条消息暂存附件（可多次附加，输入框上方显示
待发送列表，Enter 随文本一起发送）：

```text
/image:/tmp/screenshot.png
```

也可以直接 `Ctrl+V` 粘贴：剪贴板里是图片（截图工具、文件管理器复制的
图片内容等）时暂存为附件，是文本时插入输入框。支持 macOS / Windows /
X11 / Wayland。从文件管理器粘贴或拖入的图片文件路径（含 `file://` URI）
也会自动识别为附件，其余粘贴内容按普通文本插入。

启动时的 `--image` 在 TUI 模式同样生效，作为首轮消息的暂存附件。

### 上下文压缩

对话逼近模型上下文窗口时自动把较早消息压缩为结构化摘要（保留近期消息原样，
设计见 [docs/adr/0005](docs/adr/0005-context-compaction.md)）；TUI 内也可随时用
`/compact [聚焦指令]` 手动触发。压缩结果落库，resume 后保持压缩状态。
可在配置文件的 `[compaction]` 表中调整阈值（见 `config.example.toml`）。

### 日志

基于 tracing，默认写入 XDG state 目录并按天滚动：

```bash
--log file      # 默认：$XDG_STATE_HOME/nomic/logs/nomic.log.YYYY-MM-DD
                # （fallback ~/.local/state/nomic/logs）
--log terminal  # 输出到 stderr（TUI 模式下会干扰界面）
--log off       # 关闭
--log-level debug            # 过滤规则（tracing 指令语法，如 nomic=trace）
                             # 优先级：--log-level > RUST_LOG > 内置默认
```

## 配置

### 模型选择（sqlite）

nomic 的配置正从配置文件逐步迁移到 sqlite（设计见 [docs/adr/0009](docs/adr/0009-sqlite-config-model-selection.md)），当前两者共存。

**模型选择**保存在 sqlite（session 库的 `config` 表）：TUI 内 `/models` 跨 provider
选择（`<provider>/<模型id>` 格式），选择结果追加保存；启动时按
**CLI 参数 > sqlite 配置（从最新选择向最老逐条回退）** 解析，
失效的选择（provider 已删除、模型已不存在）告警后自动回退到更早的选择；
两层都没有时启动报错——没有内置默认 provider / 模型，必须显式指定。

### 配置文件

其余配置（provider 定义、连接/请求参数、压缩阈值等）在用户级配置文件
`$XDG_CONFIG_HOME/nomic/config.toml`（缺省 `~/.config/nomic/config.toml`）。
仓库根目录的 [`config.example.toml`](config.example.toml) 是带注释的完整示例，复制后按需修改。
全部字段可选；优先级为 CLI 参数 > 环境变量 > 配置文件 > 协议默认。
未知键或非法取值（reasoning）会在启动时硬报错。

```toml
# ~/.config/nomic/config.toml
base_url = "https://your.gateway/v1"
reasoning = "low"            # minimal / low / medium / high
temperature = 0.7
max_tokens = 8192
append_system = "总是用中文回复。"
# api_key = "..."           # 最低优先级兜底，建议优先用环境变量
```

启动时也可用 `--provider` / `--model` 临时指定（`--model` 支持
`<provider>/<模型id>` 全形式），优先级高于数据库中保存的选择、不写回数据库。

### 多 provider 与模型规格

`[providers.<名字>]` 定义多个 provider（没有内置 provider，`/models` 选择器
只列出配置中定义的名字），`[providers.<名字>.models."<模型id>"]`
覆盖单个模型的规格字段（全部可选，只写要覆盖的）。
provider 与 base_url 永远来自用户指定；模型规格字段逐字段按
**配置 > [models.dev](https://models.dev) > 中性兜底（全零）** 解析：

```toml
[providers.anthropic]
base_url = "https://api.anthropic.com"
# api 可省略：anthropic→anthropic_messages，openai→open_ai_completions

[providers.anthropic.models."claude-sonnet-4-5"]
reasoning = true
context_window = 200000
max_tokens = 64000
cost_input = 3.0
cost_output = 15.0
cost_cache_read = 0.3
cost_cache_write = 3.75

# 自定义 provider：api 必填
[providers.deepseek]
api = "open_ai_completions"
base_url = "https://api.deepseek.com/v1"
api_key = "sk-..."

# 只写要覆盖的字段，其余走 models.dev → 中性兜底
[providers.deepseek.models."deepseek-chat"]
max_tokens = 8192
```

models.dev 目录按模型 id 查询（约 3MB 的 api.json），缓存到
`$XDG_CACHE_HOME/nomic/models-dev-api.json`（24h 有效期，网络失败时用过期缓存兜底）；
配置已给全规格字段时不读缓存、不联网。models.dev 与缓存都不可用时回落到中性兜底值。

模型 id 必须「存在」：命中 models.dev 目录或 `[providers.*.models]` 定义之一，
否则启动与 `/models` 切换都会报错（目录不可用、无法校验时维持回落行为）。

## 上下文文件

### AGENTS.md

启动时从当前目录一路向上走到文件系统根，加载沿途每个目录的 `AGENTS.md`，
作为系统提示词的一部分注入（`<project_instructions path="...">` 块）。
按**根到叶**排序：越靠近当前目录的指令越靠后，可细化上层（如工作区级）约定。
缺失或空白文件跳过；文件不可读时告警后继续，不阻断启动。

```markdown
# 项目根 AGENTS.md 示例
- 代码改动后运行 `check`。
- 不要在本地执行生产迁移。
```

### Skills

skill 是包含 `SKILL.md` 的目录，可放在项目或用户级目录中：

```text
# 项目级（从 cwd 向上发现，越近优先级越高）
.nomic/skills/<name>/SKILL.md
.agents/skills/<name>/SKILL.md

# 用户级（项目级覆盖用户级；nomic 目录优先于通用 agent 目录）
$XDG_CONFIG_HOME/nomic/skills/<name>/SKILL.md
~/.config/nomic/skills/<name>/SKILL.md
~/.agents/skills/<name>/SKILL.md
```

`SKILL.md` 可带 frontmatter：

```markdown
---
description: Review Rust changes
triggers: [rust, review]
---

# Review steps
...
```

启动时 nomic 将 skill 的名称、描述与 triggers 注入系统提示词；模型可通过
`read` 工具按需读取完整指令：

```text
read({"path": "skill://rust-review"})
read({"path": "skill://rust-review", "offset": 20, "limit": 50})
```

也可以在启动时显式激活，完整正文会注入系统提示词：

```bash
nomic --skill rust-review
nomic -p "按 skill 审查" --skill rust-review
```

交互 TUI 中可随时手动载入，skill 正文作为一条 user 消息进入上下文（随
session 落库，resume 后仍然有效）：

```text
/skill              # 列出可用 skill
/skill:rust-review  # 载入指定 skill（输入 /skill: 后可 Tab 补全名称）
```

`skill://` 是只读资源；如需修改 skill，请显式编辑其 backing file。设计见
[docs/adr/0003](docs/adr/0003-skills-system.md)。

### Prompt Templates

prompt template 是一个 `.md` 文件，文件名（去掉 `.md`）即 `/name` 命令名，
正文是模板。输入 `/name 参数...`，模板展开为完整 prompt 后提交。

```text
# 项目级（从 cwd 向上发现，越近优先级越高）
.nomic/prompts/<name>.md

# 用户级
$XDG_CONFIG_HOME/nomic/prompts/<name>.md
~/.config/nomic/prompts/<name>.md
```

模板可带 frontmatter（`description` 缺省退化为正文第一个非空行；
`argument-hint` 只在补全弹层展示）：

```markdown
---
description: Review staged git changes
argument-hint: "<path>"
---
Review the staged changes (`git diff --cached`). Focus on:
- Bugs and logic errors
- Security issues
```

正文支持参数占位符（与 pi 对齐）：`$1`、`$2` 位置参数，`$@` / `$ARGUMENTS`
全部参数，`${1:-default}` 默认值，`${@:N}` / `${@:N:L}` 参数切片。参数可带
引号：`/component Button "click handler"`。

同名覆盖规则：`显式路径 > 项目级 > 用户级`；目录发现是非递归的。也可通过
配置文件 `prompts = [...]` 或 CLI 追加显式文件/目录，并关闭目录发现：

```bash
nomic --prompt-template prompts/review.md   # 可重复传入
nomic --no-prompt-templates                 # 关闭目录发现（显式路径仍生效）
```

print 模式同样支持：prompt 以 `/` 开头时按模板调用展开（未知名称报错）。
设计见 [docs/adr/0008](docs/adr/0008-prompt-templates.md)。

## 开发

### 开发环境

```bash
devenv shell        # 进入开发 shell（工具链版本由 rust-toolchain.toml 固定）
direnv allow        # 或使用 direnv 自动进入
```

### 本地检查

```bash
check               # 与 CI 等价的全部检查：fmt / clippy / nextest / doc / deny / machete / taplo / typos
```

每个 commit 必须通过 `check`（见 [AGENTS.md](AGENTS.md)）；CI 与本地共用
同一份 devenv 环境与检查脚本。

### 项目结构

- `crates/nomic-ai`：统一消息模型 + 流式事件协议 + provider 实现（Anthropic Messages、OpenAI Completions 兼容）
- `crates/nomic-core`：agent loop（四层生命周期事件、parallel 工具执行、hooks）+ 工具抽象（schemars + serde 即校验）+ 上下文压缩（ADR-0005）
- `crates/nomic-tools`：内建工具——`read` / `write` / `edit` / `bash`（截断、模糊匹配、BOM/CRLF 保留、文件变更队列、超时强杀进程组）、`grep` / `find`（ripgrep/fd 语义，纯库实现）、`todo_read` / `todo_write`（父子嵌套任务列表）
- `crates/nomic-session`：SQLite session 存储（树形 entries、resume、分支浏览/加载、sqlite 配置表、`sessions list`）
- `crates/nomic-skills`：skill 发现、frontmatter 元数据、覆盖规则与显式激活
- `crates/nomic-prompts`：prompt template 发现、frontmatter 元数据、覆盖规则与参数展开
- `crates/nomic-cli`：`nomic` 二进制（print 模式 + ratatui 交互 TUI + resume/sessions 子命令 + tracing 日志）
- `docs/adr/`：架构决策记录（0001–0011）

### 新增 crate

```bash
cargo new crates/<name> --lib
```

crate 的 `Cargo.toml` 中继承 workspace 配置：

```toml
[lints]
workspace = true
```

## 发布

```bash
release 0.2.0       # bump 版本 + 生成 CHANGELOG + check + 打 tag，推 tag 后 CI 自动发布
```

完整流程见 [docs/releasing.md](docs/releasing.md)；变更历史见 [CHANGELOG.md](CHANGELOG.md)。

## 路线图

已完成：

- M1：agent loop + 工具集 + print 模式（ADR-0001）
- M2（部分）：SQLite session 存储与 resume、显式分支（TUI `/tree`）、交互 TUI（ADR-0002）、用户级配置文件、上下文压缩（ADR-0005）
- M3（部分）：AGENTS.md 加载（向上发现，注入系统提示词）、skills（ADR-0003）、prompt templates（ADR-0008）
- M4：图片输入（`--image <路径>` 附件；TUI `/image <路径>` 暂存、`Ctrl+V` 剪贴板粘贴）
- 其后迭代：`grep` / `find` / `todo` 工具、跨 provider 模型选择与 sqlite 配置（ADR-0009/0010）、
  goal 模式、thinking 折叠、会话标题、vim-like 交互（ADR-0011）

待完成：

- M2（剩余）：显式 branch 创建/选择/浏览（active leaf 语义未定，见 ADR-0001 修订）、prompt caching

## License

[MIT](https://github.com/zineyu/nomic) © zineyu
