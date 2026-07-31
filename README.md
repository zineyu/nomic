# nomic

Rust 编码 agent —— [pi-coding-agent](https://github.com/badlogic/pi-mono) 的 Rust 复刻。
借鉴 pi 的核心思想（统一流式 provider 抽象、事件驱动的 agent loop、与模型的工具输出契约），
但以 Rust 风格实现（serde 即校验、`CancellationToken`、增量式流事件）。设计决策见
[docs/adr/0001](docs/adr/0001-pi-rust-architecture.md)。

## 安装

### Nix（flake）

```bash
nix run github:zine/nomic              # 直接运行
nix profile install github:zine/nomic  # 安装到 profile
```

构建工具链由 `rust-toolchain.toml` 固定，与 devenv / CI 完全一致；设计见
[docs/adr/0006](docs/adr/0006-nix-flake-packaging.md)。

### 从源码构建

```bash
cargo install --path crates/nomic-cli  # 或 cargo build --release -p nomic-cli
```

## 开发环境

```bash
devenv shell        # 进入开发 shell（工具链版本由 rust-toolchain.toml 固定）
direnv allow        # 或使用 direnv 自动进入
```

## 使用

```bash
export ANTHROPIC_API_KEY=sk-ant-...     # 或 OPENAI_API_KEY / OPENAI_BASE_URL

# 交互 TUI（缺省，设计见 docs/adr/0002）
cargo run -p nomic-cli
# 键位：Enter 发送 · Tab 补全 · Esc 取消运行 · Ctrl+C 退出 · ↑/↓/PgUp/PgDn/滚轮滚动
# 命令：/help 查看全部（/new 开启新对话，/resume 恢复历史 session，/compact 压缩上下文，/quit 退出），输入 / 自动补全

# print 模式（非交互，管道可用）
nomic -p "列出当前目录的文件"

# OpenAI 兼容端点（DeepSeek、代理网关等）
nomic -p "..." --provider openai --base-url https://your.gateway/v1 --model deepseek-chat

# 推理模型
nomic -p "..." --reasoning low

# 恢复会话（两种模式通用）
nomic --continue        # 当前目录下最近的 session（按 cwd 隔离）
nomic --session <ID>    # 指定 session（可跨目录，会有提示）
nomic resume            # 交互选择器（↑/↓ 或 j/k 移动，Enter 确认，Esc/q 取消）

# TUI 内随时可用 /resume 打开同一选择器：选中后替换当前上下文并切换落库目标

# 查看历史 session（id、最后更新时间、消息数、目录）
nomic sessions list
```

## 图片输入

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
X11 / Wayland。

启动时的 `--image` 在 TUI 模式同样生效，作为首轮消息的暂存附件。

## 上下文压缩

对话逼近模型上下文窗口时自动把较早消息压缩为结构化摘要（保留近期消息原样，
设计见 [docs/adr/0005](docs/adr/0005-context-compaction.md)）；TUI 内也可随时用
`/compact [聚焦指令]` 手动触发。压缩结果落库，resume 后保持压缩状态。
可在配置文件的 `[compaction]` 表中调整阈值（见 `config.example.toml`）。

## 配置文件

可选的用户级配置：`$XDG_CONFIG_HOME/nomic/config.toml`（缺省 `~/.config/nomic/config.toml`）。
仓库根目录的 [`config.example.toml`](config.example.toml) 是带注释的完整示例，复制后按需修改。
全部字段可选；优先级为 CLI 参数 > 环境变量 > 配置文件 > 内置默认。
未知键或非法取值（provider / reasoning）会在启动时硬报错。

```toml
# ~/.config/nomic/config.toml
provider = "openai"
model = "deepseek-chat"
base_url = "https://your.gateway/v1"
reasoning = "low"            # minimal / low / medium / high
temperature = 0.7
max_tokens = 8192
append_system = "总是用中文回复。"
# api_key = "..."           # 最低优先级兜底，建议优先用环境变量
```

### 多 provider 与模型规格

`[providers.<名字>]` 定义多个 provider，`[providers.<名字>.models."<模型id>"]`
覆盖单个模型的规格字段（全部可选，只写要覆盖的）。
provider 与 base_url 永远来自用户指定；模型规格字段逐字段按
**配置 > [models.dev](https://models.dev) > 内置默认** 解析：

```toml
provider = "deepseek"
model = "deepseek-chat"

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

# 只写要覆盖的字段，其余走 models.dev → 内置默认
[providers.deepseek.models."deepseek-chat"]
max_tokens = 8192
```

models.dev 目录按模型 id 查询（约 3MB 的 api.json），缓存到
`$XDG_CACHE_HOME/nomic/models-dev-api.json`（24h 有效期，网络失败时用过期缓存兜底）；
配置已给全规格字段时不读缓存、不联网。models.dev 与缓存都不可用时回落到内置默认值。

## AGENTS.md

启动时从当前目录一路向上走到文件系统根，加载沿途每个目录的 `AGENTS.md`，
作为系统提示词的一部分注入（`<project_instructions path="...">` 块）。
按**根到叶**排序：越靠近当前目录的指令越靠后，可细化上层（如工作区级）约定。
缺失或空白文件跳过；文件不可读时告警后继续，不阻断启动。

```markdown
# 项目根 AGENTS.md 示例
- 代码改动后运行 `check`。
- 不要在本地执行生产迁移。
```

## Skills

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

## 本地检查

```bash
check               # 与 CI 等价的全部检查：fmt / clippy / nextest / doc / deny / machete / taplo / typos
```

## 结构

- `crates/nomic-ai`：统一消息模型 + 流式事件协议 + provider 实现（Anthropic Messages、OpenAI Completions 兼容）
- `crates/nomic-core`：agent loop（四层生命周期事件、parallel 工具执行、hooks）+ 工具抽象（schemars + serde 即校验）+ 上下文压缩（ADR-0005）
- `crates/nomic-tools`：read/write/edit/bash 四工具（截断、模糊匹配、BOM/CRLF 保留、文件变更队列）
- `crates/nomic-session`：SQLite session 存储（树形 entries、resume、`sessions list`）
- `crates/nomic-skills`：skill 发现、frontmatter 元数据、覆盖规则与显式激活
- `crates/nomic-cli`：`nomic` 二进制（print 模式 + ratatui 交互 TUI + sessions 子命令）
- `docs/adr/`：架构决策记录

## 路线图

已完成：

- M1：agent loop + 四工具 + print 模式（ADR-0001）
- M2（部分）：SQLite session 存储与 resume（树形 schema 已就位）、交互 TUI（ADR-0002）、用户级配置文件、上下文压缩（ADR-0005）
- M3（部分）：AGENTS.md 加载（向上发现，注入系统提示词）、skills（ADR-0003）
- M4：图片输入（`--image <路径>` 附件；TUI `/image <路径>` 为下一条消息暂存图片）

待完成：

- M2（剩余）：显式 branch 创建/选择/浏览（active leaf 语义未定，见 ADR-0001 修订）、prompt caching
- M3（剩余）：compaction、prompt templates

## 新增 crate

```bash
cargo new crates/<name> --lib
```

crate 的 `Cargo.toml` 中继承 workspace 配置：

```toml
[lints]
workspace = true
```
