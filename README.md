# nomic

Rust 编码 agent —— [pi-coding-agent](https://github.com/badlogic/pi-mono) 的 Rust 复刻。
借鉴 pi 的核心思想（统一流式 provider 抽象、事件驱动的 agent loop、与模型的工具输出契约），
但以 Rust 风格实现（serde 即校验、`CancellationToken`、增量式流事件）。设计决策见
[docs/adr/0001](docs/adr/0001-pi-rust-architecture.md)。

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
# 键位：Enter 发送 · Esc 取消运行 · Ctrl+C 退出 · ↑/↓/PgUp/PgDn/滚轮滚动

# print 模式（非交互，管道可用）
nomic -p "列出当前目录的文件"

# OpenAI 兼容端点（DeepSeek、代理网关等）
nomic -p "..." --provider openai --base-url https://your.gateway/v1 --model deepseek-chat

# 推理模型
nomic -p "..." --reasoning low

# 恢复会话（两种模式通用）
nomic --continue        # 当前目录下最近的 session（按 cwd 隔离）
nomic --session <ID>    # 指定 session（可跨目录，会有提示）

# 查看历史 session（id、最后更新时间、消息数、目录）
nomic sessions list
```

## 配置文件

可选的用户级配置：`$XDG_CONFIG_HOME/nomic/config.toml`（缺省 `~/.config/nomic/config.toml`）。
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

## 本地检查

```bash
check               # 与 CI 等价的全部检查：fmt / clippy / nextest / doc / deny / machete / taplo / typos
```

## 结构

- `crates/nomic-ai`：统一消息模型 + 流式事件协议 + provider 实现（Anthropic Messages、OpenAI Completions 兼容）
- `crates/nomic-core`：agent loop（四层生命周期事件、parallel 工具执行、hooks）+ 工具抽象（schemars + serde 即校验）
- `crates/nomic-tools`：read/write/edit/bash 四工具（截断、模糊匹配、BOM/CRLF 保留、文件变更队列）
- `crates/nomic-session`：SQLite session 存储（树形 entries、resume、`sessions list`）
- `crates/nomic-cli`：`nomic` 二进制（print 模式 + ratatui 交互 TUI + sessions 子命令）
- `docs/adr/`：架构决策记录

## 路线图

已完成：

- M1：agent loop + 四工具 + print 模式（ADR-0001）
- M2（部分）：SQLite session 存储与 resume（树形 schema 已就位）、交互 TUI（ADR-0002）、用户级配置文件
- M3（部分）：AGENTS.md 加载（向上发现，注入系统提示词）

待完成：

- M2（剩余）：显式 branch 创建/选择/浏览（active leaf 语义未定，见 ADR-0001 修订）、prompt caching
- M3（剩余）：compaction、skills / prompt templates
- M4：图片输入（provider 与消息类型已预留，缺 CLI/agent 入口）

## 新增 crate

```bash
cargo new crates/<name> --lib
```

crate 的 `Cargo.toml` 中继承 workspace 配置：

```toml
[lints]
workspace = true
```
