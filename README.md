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
nomic --continue        # 最近一次 session
nomic --session <ID>    # 指定 session
```

## 本地检查

```bash
check               # 与 CI 等价的全部检查：fmt / clippy / nextest / doc / deny / machete / taplo / typos
```

## 结构

- `crates/nomic-ai`：统一消息模型 + 流式事件协议 + provider 实现（Anthropic Messages、OpenAI Completions 兼容）
- `crates/nomic-core`：agent loop（四层生命周期事件、parallel 工具执行、hooks）+ 工具抽象（schemars + serde 即校验）
- `crates/nomic-tools`：read/write/edit/bash 四工具（截断、模糊匹配、BOM/CRLF 保留、文件变更队列）
- `crates/nomic-cli`：`nomic` 二进制（print 模式 + ratatui 交互 TUI）
- `docs/adr/`：架构决策记录

## 路线图（ADR-0001 锚定）

- M2：SQLite session（树结构 + branching）、prompt caching
- M3：compaction、skills / prompt templates / AGENTS.md 加载
- M4：图片输入

## 新增 crate

```bash
cargo new crates/<name> --lib
```

crate 的 `Cargo.toml` 中继承 workspace 配置：

```toml
[lints]
workspace = true
```
