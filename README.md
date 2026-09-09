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
  模型分层解析（配置 > [models.dev](https://models.dev) > 中性兜底）
- **双运行模式**：ratatui 全屏交互 TUI（单字母动作层，[ADR-0021](docs/adr/0021-single-letter-action-layer.md)）+ 非交互 print 模式（管道可用）
- **Flutter GUI**：`--serve` 启动 headless 事件流服务（纯 WebSocket 协议），Flutter 桌面应用（`app/`）连接使用（[ADR-0046](docs/adr/0046-flutter-gui.md)）
- **排队输入**：运行中 `Enter` 把消息排入统一消息队列（当前步骤完成后注入本轮运行，
  未清空则持续续行；运行被取消或失败时队列保留，恢复后按序作为下一轮发送）；
  oil.nvim 式 QUEUE 模式编辑队列（就地编辑/删除/换位），设计见
  [ADR-0014](docs/adr/0014-unified-message-queue.md)
- **运行状态提示**：输入框上方单行阶段提示（`thinking...` / `tool calling...` /
  `writing...` / `waiting...`），文字带自左到右的扫光动效
- **九件工具**：`read` / `write` / `edit` / `bash` / `grep` / `find` / `todo_read` /
  `todo_write` / `ask_user_question`（单选/多选/填空，自动追加自定义填写选项，
  TUI 弹出模态提问框），schemars + serde 即校验，parallel 执行
- **持久会话**：SQLite 树形存储，归属链 project 1—N work 1—N session（ADR-0044：
  project 对应一个 git repo；work 为一次任务的完整协作过程，是用户可见的一等入口），
  工具与 mention 的相对路径以 session 所属 project 为基准；支持 resume（按 project
  隔离）、会话分支浏览与创建、`sessions list`
- **上下文工程**：AGENTS.md 向上发现注入、skills 系统、prompt templates、自动/手动上下文压缩
- **图片输入**：`--image` 附件、`image` 命令暂存、`Ctrl+V` 剪贴板图片粘贴
- **外部编辑器**：INSERT 下 `Ctrl+G` 挂起 TUI，用 `$VISUAL`/`$EDITOR`
  （缺省 `vi`）编辑当前草稿（长文/多行 prompt），保存退出后写回；
  编辑器异常退出或内容为空时原草稿保留，
  设计见 [ADR-0017](docs/adr/0017-external-editor.md)
- **工程化**：devenv 统一开发环境，`check` 一键执行与 CI 等价的全部检查，Nix flake 打包

## 目录

- [安装](#安装)
- [快速上手](#快速上手)
- [使用](#使用)
  - [术语](#术语)
  - [运行模式](#运行模式)
  - [TUI 键位](#tui-键位)
  - [TUI 命令](#tui-命令)
  - [会话恢复与分支](#会话恢复与分支)
  - [图片输入](#图片输入)
  - [上下文压缩](#上下文压缩)
  - [日志](#日志)
- [配置](#配置)
  - [模型选择（sqlite）](#模型选择sqlite)
  - [配置文件](#配置文件)
  - [多 provider 与模型](#多-provider-与模型)
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
cargo install --path crates/app/nomic-cli  # 或 cargo build --release -p nomic-cli
```

## 快速上手

```bash
# 1. 配置 API key
export ANTHROPIC_API_KEY=sk-ant-...     # 或 OPENAI_API_KEY / OPENAI_BASE_URL

# 2. 首次启动必须显式指定模型（无内置默认 provider / 模型）
nomic --model anthropic/claude-sonnet-4-5

# 3. 之后在 TUI 内经 models 命令切换，选择结果跨会话记住
```

## 使用

### 术语

三层模型（[ADR-0044](docs/adr/0044-terminology-project-work-session.md)）：

| 术语 | 定义 |
| --- | --- |
| **project** | 一个 git 仓库目录：AGENTS.md / skills 发现、工具相对路径与 session 隔离的基准 |
| **work** | 一次任务协作过程：侧栏与 `sessions list` / `resume` 的一等入口，session 的分组容器 |
| **session** | 一次 agent 交互上下文：消息流、compaction 与分支的最小持久化单元 |

层级为 project 1—N work 1—N session；多 agent 协作（`create_agent`）的子
agent 落库为同一 work 下的子 session，可在侧栏展开回溯（只读）。

### 运行模式

```bash
# 交互 TUI（缺省，设计见 docs/adr/0002）
nomic --model anthropic/claude-sonnet-4-5

# print 模式（非交互，流式输出到 stdout，管道可用）
nomic -p "列出当前目录的文件"

# Flutter GUI（先启动 headless 事件流服务，缺省绑定 127.0.0.1:3333）
nomic --serve
# 再启动 GUI（devenv 内：`app-dev`，等价于 cd app && flutter run -d macos）
# 指定端口 / 监听地址（跨机访问自担风险）
nomic --serve --port 8080 --host 127.0.0.1

# OpenAI 兼容端点（DeepSeek、代理网关等）
nomic -p "..." --provider openai --base-url https://your.gateway/v1 --model deepseek-chat

# 推理模型
nomic -p "..." --reasoning low
# TUI 内：models 命令选择模型后可为推理模型选择思考级别（含 off 关闭）

# 指定工作目录（session 隔离、AGENTS.md/skills 发现、工具相对路径均基于它；
# 其余相对路径参数如 --image 也按该目录解析）
nomic --cwd /path/to/project
```

### TUI 键位

交互为「双态 + 叠加层」：INSERT 输入（默认），NORMAL 为纯浏览态的单字母动作层
（less/lazygit 式，设计见 [ADR-0021](docs/adr/0021-single-letter-action-layer.md)，
纯浏览化见 [ADR-0026](docs/adr/0026-normal-browse-only.md)）；`Esc` 逐层退回。

| 模式 | 键位 | 说明 |
| ---- | ---- | ---- |
| INSERT | `Enter` | 发送消息（运行中排入队列，当前步骤完成后注入本轮）；`/` 开头按普通文本发送，不触发命令 |
| INSERT | `Esc` | 进入 NORMAL（运行中亦然，Esc 永不中断运行；中断在 NORMAL 按 `q`） |
| INSERT | `Ctrl+W` `Ctrl+U` `Ctrl+A/E` `Alt+B/F` | 删词 / 清行 / 行首行尾 / 词移动 |
| INSERT | `Ctrl+C` / `Ctrl+D` | 清草稿（再按退出）/ 空草稿退出、非空删字符 |
| INSERT | `↑/↓` | 输入历史召回（提交过的 prompt，↓ 到底还原暂存草稿） |
| INSERT | `Ctrl+G` | 外部编辑器（`$VISUAL`/`$EDITOR`，缺省 `vi`）编辑当前草稿；保存退出后写回，异常退出或内容为空时原草稿保留 |
| INSERT | `Ctrl+V` | 粘贴剪贴板图片 |
| NORMAL | `j` `k` `d` `u` `g` `G` | 滚动 / 半页下上 / 顶部 / 底部（less 式） |
| NORMAL | `Y` | 直接复制最新一条消息 |
| NORMAL | `m` `r` | 队列编辑 / 重试最近一轮 |
| NORMAL | `s` `b` `c` | 恢复会话 / 会话树（创建分支）/ 新建会话 |
| NORMAL | `e` `:` `?` | 外部编辑器 / 浮层命令栏 / 帮助弹层 |
| NORMAL | `q` | 中断本轮运行（退出程序用 `:→quit` 命令，或 `Ctrl+C` 硬退出） |
| NORMAL | `i` `a` `A` `I` `Enter` | 回到输入（光标原位 / 末尾 / 行首） |
| COMMAND | `Enter` / `Tab` | 执行命令 / 补全命令、模板、skill |
| COMMAND | `Esc` | 关补全弹层 / 放弃返回 NORMAL |
| QUEUE | `j` `k` `g` `G` | 移动条目游标 / 队首 / 队尾 |
| QUEUE | `i` `o` `O` `Enter` | 就地编辑 / 下方新增 / 上方新增（`Enter`/`Esc` 保存，空文本即删条目） |
| QUEUE | `dd` `x` `J` `K` | 删除条目 / 下移 / 上移（换位）；打开期间冻结发送，退出恢复 |
| 提问 | `↑/↓`（`j/k`） | 移动选项（`ask_user_question` 弹层；单选/多选问题自动带「自定义填写」选项） |
| 提问 | `空格` / `Enter` | 多选勾选 / 提交（单选直接 `Enter` 提交；自定义选项先输入文本再提交） |
| 提问 | `Esc` | 取消提问（自定义输入阶段先放弃回选项列表） |
| picker | 输入即过滤 · `↑/↓` 选择 · `Home/End` 首尾 | 适用于 `resume`、`models`、`tree` 命令 |
| 通用 | `PgUp/PgDn` 滚轮 | 滚动聊天区（不切态） |
| 通用 | `Shift`+拖选 | 复制文本（TUI 捕获鼠标用于滚轮，原生选择需按住 Shift） |

### TUI 命令

命令只在浮层命令栏执行（ADR-0020）：NORMAL 下按 `:` 在屏幕中上方打开单行命令栏，命令无 `/` 前缀，Tab 补全；`help` 查看全部。聊天输入框（INSERT）不触发命令，`/` 开头的输入按普通文本发送。

| 命令 | 说明 |
| ---- | ---- |
| `help` | 显示可用命令 |
| `new` | 清空上下文，开启新对话（新 session） |
| `resume` | 交互选择并恢复历史 work 的主 session（切换上下文与落库目标） |
| `tree` | 浏览会话树，选择非工具调用条目作为新分支起点（原分支保留） |
| `models` | 跨 provider 切换模型（`models:<provider>/<模型id>` 亦可）；推理模型联动选择思考级别 |
| `skill` `skill:<name>[ args]` | 列出可用 skill / 手动载入指定 skill（可附加上下文） |
| `image:<路径>` | 为下一条消息附加图片（可多次附加） |
| `compact [聚焦指令]` | 手动压缩上下文 |
| `continue` | 续跑：重发最后一条消息（user 消息或 tool result）重新请求模型 |
| `copy` | 复制最新一条消息到剪贴板 |
| `thinking` | 切换 thinking 内容折叠/展开显示 |
| `goal <目标>` | 目标驱动运行：agent 持续工作直到调用 `goal_done` 汇报完成（期间换出 `ask_user_question`）；react loop 停止而目标未完成时自动复述目标继续追问；`goal` 无参取消进行中的目标 |
| `config <子命令>` | 查看/修改设置（`config set temperature 0.7`、`config providers set anthropic --api-key sk-...` 等，与 CLI 同一命令树） |
| `quit`（`exit`） | 退出 TUI |

运行中本地命令（`help`、`copy` 等）照常可用，不被工具调用阻塞。
运行中输入的普通消息按 `Enter` 排入统一消息队列（见上「排队输入」）；
命令栏提交的模板调用同样入队；会话命令（`compact`、`continue`、`models` 等）仍须等本轮结束。

### Flutter GUI（`--serve` + `app/`）

`nomic --serve` 启动 headless 事件流服务（缺省 `127.0.0.1:3333`），GUI 为
`app/` 下的 Flutter 桌面应用（macOS 优先），经 `ws://{host}/ws` 连接：

```bash
nomic --serve [--port N] [--host H]   # 终端 1：事件流服务
app-dev                               # 终端 2（devenv）：cd app && flutter run -d macos
```

- **流式聊天**：markdown 渲染、工具调用卡片；运行中发送的消息进入统一消息
  队列，当前步骤完成后注入本轮运行（与 TUI 同一语义）
- **会话管理**：启动页选择 project 后开始新 work（无默认 project，work 严格
  归属选定目录，连带创建主 session）；work/session 列表恢复历史会话
  （复用 SQLite 存储，与 TUI/print 共用）
- **模型选择**：跨 provider 候选列表；切换结果落库，与 TUI `/models` 同一口径
- **提问**：`ask_user_question` 以弹层呈现（单选/多选/填空 + 自定义填写）
- `--host` 缺省 `127.0.0.1`（本服务能执行 bash，跨机访问自担风险）；
  WebSocket 校验 `Origin`（浏览器场景防护），不开放 CORS

设计见 [docs/adr/0046](docs/adr/0046-flutter-gui.md)（协议部分见
[docs/adr/0030](docs/adr/0030-web-ui.md)）。

### 会话恢复与分支

```bash
nomic --continue        # 当前目录对应 project 下最近的 work（按 project 隔离），恢复其主 session
nomic --session <ID>    # 指定 session（可跨目录，会有提示）
nomic resume            # 交互选择 work（↑/↓ 或 j/k 移动，Enter 确认，Esc/q 取消）

# 查看历史 work（标题、最后更新时间、消息数、所属 project）
nomic sessions list
```

- TUI 内随时可用 `resume` 命令打开同一选择器：选中 work 后恢复其主 session，
  替换当前上下文并切换落库目标。
- 会话分支：`tree` 命令浏览当前 session 的消息树，选择非工具调用条目作为新分支起点——
  上下文回到该条目，后续对话写入新分支，原分支保留可回访。

### 图片输入

print 模式用 `--image` 附带图片（可重复；png/jpeg/gif/webp）：

```bash
nomic -p "这张截图里有什么错误" --image screenshot.png
```

交互 TUI 用 `image` 命令为下一条消息暂存附件（可多次附加，输入框上方显示
待发送列表，Enter 随文本一起发送）：

```text
image:/tmp/screenshot.png
```

也可以直接 `Ctrl+V` 粘贴：剪贴板里是图片（截图工具、文件管理器复制的
图片内容等）时暂存为附件，是文本时插入输入框。支持 macOS / Windows /
X11 / Wayland。从文件管理器粘贴或拖入的图片文件路径（含 `file://` URI）
也会自动识别为附件，其余粘贴内容按普通文本插入。

启动时的 `--image` 在 TUI 模式同样生效，作为首轮消息的暂存附件。

### 上下文压缩

对话逼近模型上下文窗口时自动把较早消息压缩为结构化摘要（保留近期消息原样，
设计见 [docs/adr/0005](docs/adr/0005-context-compaction.md)）；TUI 内也可随时用
`compact [聚焦指令]` 手动触发。压缩结果落库，resume 后保持压缩状态。
阈值经设置调整（`nomic config set compaction.reserve_tokens <N>`，见下文「配置」）。

### 日志

基于 tracing，默认写入平台标准 state 目录（由 [`dirs`](https://docs.rs/dirs) 解析）并按天滚动：

```bash
--log file      # 默认：<state 目录>/nomic/logs/nomic.log.YYYY-MM-DD
                # Linux：$XDG_STATE_HOME（缺省 ~/.local/state）；
                # 其他平台无 state 目录定义，回退 data 目录
                # （macOS：~/Library/Application Support）
--log terminal  # 输出到 stderr（TUI 模式下会干扰界面）
--log off       # 关闭
--log-level debug            # 过滤规则（tracing 指令语法，如 nomic=trace）
                             # 优先级：--log-level > RUST_LOG > 内置默认
```

## 配置

所有设置保存在 sqlite（session 库的 providers / model_specs / settings 表；
设计见 [docs/adr/0039](docs/adr/0039-settings-in-sqlite.md)），**不再有配置文件**：
`config.toml` 被完全忽略（不读取、不迁移、不报错），用户在以下三个入口
重建配置，同一套命令语义：

- **CLI**：`nomic config ...`（`config-path` 查看数据库位置）；
- **TUI**：命令栏 `config ...`（无 `/` 前缀，`help` 可查）；
- **Flutter GUI**：设置页（providers / 模型覆盖 / 标量可视化编辑；走 WS 设置事件）。

优先级统一为 **CLI 参数 > 环境变量 > sqlite > 协议/内置默认**；
写路径对未知键与非法取值硬报错。

### 标量设置

`nomic config list|get|set|unset`（value 先按 JSON 解析，失败按字符串）：

| key | 取值 | 说明 |
| --- | ---- | ---- |
| `reasoning` | minimal / low / medium / high | 默认思考级别（仅推理模型生效） |
| `append_system` | 字符串 | 追加到系统提示词的文本 |
| `prompts` | JSON 数组 | 额外 prompt template 路径 |
| `compaction.enabled` | 布尔 | 自动压缩开关 |
| `model_aliases` | JSON 对象 | 模型别名表（别名 → `<provider>/<模型id>`），子 agent 与 `models:` 切换可用 |

> `temperature` / `max_tokens` 由 CLI 参数（`--temperature` / `--max-tokens`）提供；
> `base_url` / `api_key` 走 provider 定义与环境变量；压缩的 token 参数使用内置默认值。

### Providers 与模型

不预置任何凭据：`nomic config providers set anthropic --api-key sk-...` 添加
（`anthropic` / `openai` 可按名推断 API 种类；其余名字需 `--api` 显式指定）。
api_key 建议优先用环境变量（`ANTHROPIC_API_KEY` / `OPENAI_API_KEY` /
`KIMI_API_KEY`）或启动时的 `--api-key`，避免明文落库。

Kimi（Moonshot）用专属 API 种类 `kimi_completions`（OpenAI Completions 协议 +
MFJS 工具 schema 子集；base_url 缺省为 Kimi For Coding 订阅端点
`https://api.kimi.com/coding/v1`，api_key 环境变量 `KIMI_API_KEY`；provider
名任意，模型规格经 models.dev 目录 `kimi-for-coding` 命中）：

```bash
nomic config providers set kimi --api kimi_completions --api-key sk-...
nomic --model kimi/k3
```

模型覆盖用 `nomic config models set <provider> <模型id> [--context-window N ...]`：
models.dev 目录缺字段或需修正时逐字段覆盖（没写的字段不覆盖）。规格逐字段按
**sqlite 覆盖 > [models.dev](https://models.dev) > 中性兜底（全零）** 解析：

```bash
nomic config models set anthropic claude-sonnet-4-5 --context-window 200000 \
    --max-tokens 64000 --cost-input 3.0 --cost-output 15.0
nomic config providers set deepseek --api open_ai_completions \
    --base-url https://api.deepseek.com/v1 --api-key sk-...
```

models.dev 目录按模型 id 查询（约 3MB 的 api.json），缓存到平台标准 cache 目录下的
`nomic/models-dev-api.json`（Linux：`$XDG_CACHE_HOME`，缺省 `~/.cache`；macOS：`~/Library/Caches`；
24h 有效期，网络失败时用过期缓存兜底）；
规格覆盖已给全字段时不读缓存、不联网。models.dev 与缓存都不可用时回落到中性兜底值。

模型 id 必须「存在」：命中 models.dev 目录或 sqlite 规格覆盖之一，
否则启动与 `models` 切换都会报错（目录不可用、无法校验时维持回落行为）。

### 模型选择（sqlite）

模型选择保存在 sqlite（append-only `config` 表，
设计见 [docs/adr/0009](docs/adr/0009-sqlite-config-model-selection.md)）：
TUI 内 `models` 命令跨 provider 选择（`<provider>/<模型id>` 格式），
选择结果追加保存；启动时按
**CLI 参数 > sqlite 配置（从最新选择向最老逐条回退）** 解析，
失效的选择（provider 已删除、模型已不存在）告警后自动回退到更早的选择；
两层都没有可用选择时不再启动失败——TUI / serve 以占位模型照常启动，
发消息时提示先经 `models:<provider>/<模型id>` 或设置页完成选择
（无内置默认 provider / 模型）；print 模式非交互，仍在启动时报错。
启动时也可用 `--provider` / `--model` 临时指定（`--model` 支持
`<provider>/<模型id>` 全形式），优先级高于数据库中保存的选择、不写回数据库。

## 上下文文件

### AGENTS.md

从 session 的 project（操作基准目录）一路向上走到文件系统根，加载沿途
每个目录的 `AGENTS.md`，作为系统提示词的一部分注入
（`<project_instructions path="...">` 块）。按**根到叶**排序：越靠近
project 的指令越靠后，可细化上层（如项目级）约定。
缺失或空白文件跳过；文件不可读时告警后继续，不阻断启动。

启动、`--session` 跨目录恢复、TUI `/resume` 切换与 GUI 按 project 创建
session 时，提示词（含末尾的 cwd 脚注）都以该 session 的 project 为基准
构建——与工具相对路径同一口径（project 严格归属）。

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
# <config 目录>：Linux 为 $XDG_CONFIG_HOME（缺省 ~/.config），
#   macOS 为 ~/Library/Application Support
<config 目录>/nomic/skills/<name>/SKILL.md
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

另有三个控制可见性的可选字段：

- `enabled: false`：整个 skill 不可用（catalog 与 `skill://` 均跳过）；
- `hide: true`：不出现在系统提示词清单，仍可 `--skill` / `skill:<name>` 命令
  显式调用（适合只供显式触发的 skill）。

skill 目录可携带附属文件（`scripts/`、`references/` 等），通过子路径读取：

```text
read({"path": "skill://rust-review/references/checklist.md"})  # 读文件
read({"path": "skill://rust-review/scripts"})                   # 列目录
```

子路径被限制在 skill 目录内（拒绝 `..` 穿越与绝对路径）；显式激活时注入块会
附带 `[Skill directory: ...]` 指引，正文中引用的相对路径以该目录为基准解析。

#### 内部 URI 与选择器

`skill://` 是 nomic 的内部 URI 协议（设计与演进见
[ADR-0040](docs/adr/0040-internal-uri-router.md)）：`read` 按 scheme 分发到对应
协议 handler；`write`/`edit` 对只读协议（含 `skill://`）直接报错。内部 URI 支持
尾挂**行选择器**（对文件系统路径不生效，那里请用 `offset`/`limit` 参数）：

```text
read({"path": "skill://rust-review:50-100"})                 # 第 50–100 行
read({"path": "skill://rust-review:5-16,960-973"})           # 多段
read({"path": "skill://rust-review:raw"})                    # 原文
read({"path": "skill://x/a.md:conflicts?theirs=b.md"})       # 与 b.md 的三方冲突区域（可附 &base=…）
```

选择器畸形即报错（如 `:0`、`:-3`），不会静默放宽为全量读取。

在 TUI 的输入框里，`@skill://<name>` 可作为 mention 引用 skill，发送时展开为
内容块。

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
/skill                        # 列出可用 skill
/skill:rust-review            # 载入指定 skill（输入 /skill: 后可 Tab 补全名称）
/skill:rust-review 只看 unsafe  # 名称后空白起为附加上下文，以 User: <args> 追加
```

聊天草稿里也可用 `@` mention 内联引用 skill 或文件，随消息一起发送（输入
`@` 后 Tab 补全，`@skill://` 后补全 skill 名、`@file:` 后补全文件路径）：

```text
用 @skill://rust-review 审查，参考 @file:src/main.rs
```

`@` 只是标记，不立即注入；Enter 发送时才把**有效**的 skill / 文件内容展开
进消息（无效标记原样发送，不阻断）。展开块在聊天区折叠为紧凑标记展示。

`skill://` 是只读资源；如需修改 skill，请显式编辑其 backing file。设计见
[docs/adr/0003](docs/adr/0003-skills-system.md)。

### Prompt Templates

prompt template 是一个 `.md` 文件，文件名（去掉 `.md`）即命令名，
正文是模板。在浮层命令栏（NORMAL `:`）输入 `name 参数...`，模板展开为
完整 prompt 后提交。

```text
# 项目级（从 cwd 向上发现，越近优先级越高）
.nomic/prompts/<name>.md

# 用户级（<config 目录> 规则同上）
<config 目录>/nomic/prompts/<name>.md
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
check               # 与 CI 等价的全部检查：fmt / clippy / nextest / doc / deny /
                    # machete / taplo / typos，末尾含 app/ Flutter 检查
                    #（pub get → dart format → analyze → test）
```

每个 commit 必须通过 `check`（见 [AGENTS.md](AGENTS.md)）；CI 与本地共用
同一份 devenv 环境与检查脚本。

App 侧也可单独执行：`app-check`（完整）、`app-dev`（flutter run，需先
`nomic --serve`）。

### 项目结构

`crates/` 按层级分为两组：`runtime/` 是可复用的 agent harness（零 nomic 品牌概念），
`app/` 是 nomic 产品自身。依赖方向单向：`app → runtime`。

- `crates/runtime/nomic-ai`：统一消息模型 + 流式事件协议 + provider 实现（Anthropic Messages、OpenAI Completions 兼容）
- `crates/runtime/nomic-core`：agent loop（四层生命周期事件、parallel 工具执行、事件拦截 event interception）+ 工具抽象（schemars + serde 即校验）+ 上下文压缩（ADR-0005）
- `crates/runtime/nomic-session`：SQLite session 存储（树形 entries、resume、分支浏览/加载、sqlite 配置表、`sessions list`）
- `crates/app/nomic-skills`：skill 发现、frontmatter 元数据、覆盖规则与显式激活
- `crates/app/nomic-prompts`：prompt template 发现、frontmatter 元数据、覆盖规则与参数展开
- `crates/app/nomic-tools`：内建工具——`read` / `write` / `edit` / `bash`（截断、模糊匹配、BOM/CRLF 保留、文件变更队列、超时强杀进程组）、`grep` / `find`（基于 fff 常驻索引，纯库实现）、`todo_read` / `todo_write`（父子嵌套任务列表）
- `crates/app/nomic-cli`：`nomic` 二进制（print 模式 + ratatui 交互 TUI + `--serve` headless 事件流服务 + resume/sessions 子命令 + tracing 日志）
- `app/`：Flutter GUI（桌面应用；经 `ws://{host}/ws` 连接 `nomic --serve`，协议见 ADR-0030/0046）
- `docs/adr/`：架构决策记录（0001–0030）

### 新增 crate

```bash
cargo new crates/runtime/<name> --lib   # 或 crates/app/<name>
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
  目标驱动运行（goal 命令，ADR-0037）、thinking 折叠、会话标题、单字母动作层交互（ADR-0011 → ADR-0021）

待完成：

- M2（剩余）：显式 branch 创建/选择/浏览（active leaf 语义未定，见 ADR-0001 修订）、prompt caching

## License

[MIT](https://github.com/zineyu/nomic) © zineyu
