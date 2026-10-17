# TODO

记录工作中发现、但不在当前 change 范围内的改进与重构。

## 规则

- 每条 TODO 使用固定模板：范围、优先级、发现时间、现状、问题、期望结果、建议方案、验收标准、相关文件。
- 「现状」与「问题」只写已验证的事实。禁止写入推测。
- 完成后删除条目。本文件不保留完成历史。
- 当前 change 范围内的修复直接实施，不进入本文件。
- 优先级：P0 影响正确性，P1 影响可维护性，P2 改善体验。
- 新增条目追加到末尾，编号递增。

## 模板

### T-XXX 一句话标题

- 范围：
- 优先级：
- 发现时间：YYYY-MM-DD
- 现状：
- 问题：
- 期望结果：
- 建议方案：
- 验收标准：
- 相关文件：

## 条目

### T-001 清理配置中已移除组件的引用

- 范围：仓库配置
- 优先级：P2
- 发现时间：2026-10-18
- 现状：`.gitignore` 包含 9 条 Flutter `app/` 规则与 `/crates/app/nomic-cli/.nomic/`；`taplo.toml` 排除 3 条 `app/**`；`_typos.toml` 排除 3 条 `app/**`；`scripts/file-size-baseline.txt` 的注释条目引用 `crates/nomic-cli`、`crates/nomic-prompts`、`crates/nomic-skills`、`crates/nomic-ai/src/providers/*` 等路径。仓库当前只有 4 个 crate：`crates/core/nomic-core`、`crates/ai/nomic-ai`、`crates/server/nomic-server`、`crates/tui/nomic-tui`。
- 问题：这些规则与条目不会命中任何文件，但会误导读者；`file-size-baseline.txt` 的注释条目让棘轮清单看起来包含有效豁免。
- 期望结果：配置只引用当前仓库存在的路径与组件。
- 建议方案：删除 `app/` 相关的 ignore 与 exclude 规则；删除 baseline 中已不存在路径的条目，保留文件头的格式说明。
- 验收标准：`rg 'app/' .gitignore taplo.toml _typos.toml` 无匹配；`scripts/file-size-baseline.txt` 的每个路径都存在于仓库；`check` 通过。
- 相关文件：.gitignore, taplo.toml, _typos.toml, scripts/file-size-baseline.txt

### T-002 收敛 deny.toml 的豁免与许可证白名单

- 范围：依赖策略
- 优先级：P1
- 发现时间：2026-10-18
- 现状：`cargo deny check` 报告 `RUSTSEC-2025-0141` 未命中任何 crate；8 条许可证白名单（BSD-2-Clause、BSD-3-Clause、BSL-1.0、CC0-1.0、CDLA-Permissive-2.0、ISC、MPL-2.0、Zlib）未被任何依赖使用。`Cargo.lock` 中不存在 `bincode`、`fff-search`、`heed`。
- 问题：未命中的 advisory 豁免在该 crate 重新出现时静默放行；未使用的许可证白名单扩大允许范围，新增依赖的许可证变化不会在检查中暴露。
- 期望结果：白名单与豁免只覆盖当前依赖图实际需要的项。
- 建议方案：删除未命中的 advisory 豁免；把许可证白名单收敛到实际遇到的许可证；把 `licenses.unused-allowed-license` 设为 `deny`，使新增许可证必须显式评审。
- 验收标准：`cargo deny check` 不再输出 `advisory-not-detected` 与 `license-not-encountered` 警告；引入未白名单的许可证时检查失败。
- 相关文件：deny.toml

### T-003 会话状态改用 typestate

- 范围：nomic-core 领域模型（涉及 server）
- 优先级：P1
- 发现时间：2026-10-18
- 现状：`SessionState` 是运行时枚举；`dispatch`、`suspend`、`quiesce` 返回 `Result<Self, StateTransitionError>`；`Session` 用私有字段 `state: SessionState` 保存当前状态；存储层经 `set_state`、`claim_session`、`quiesce_session` 用运行时值与 `Result`/`bool` 表达状态。
- 问题：标准第 4.7 条要求时序受限的状态使用 typestate。当前实现在运行时校验时序，非法转换只有执行到才失败。
- 期望结果：状态是类型（Pending / Running / Idle 标记）；仅合法操作在对应状态可见；非法转换在编译期拒绝。
- 建议方案：以 `Session<S>` 泛型承载状态标记；转换方法消耗 `self` 并返回下一状态类型；存储实现在读取边界把持久化状态解析为对应标记；`StateTransitionError` 只保留给外部输入解析失败路径。
- 验收标准：核心路径不再出现运行时状态转换 API；崩溃重驱、多客户端串行化的验收测试全部通过；对同一状态重复 `dispatch` 的调用无法通过编译。
- 相关文件：crates/core/nomic-core/src/session.rs, crates/core/nomic-core/src/storage.rs, crates/server/nomic-server/src/agent/worker.rs, crates/server/nomic-server/src/storage/sqlite.rs

### T-004 领域类型收敛到 newtype 与行为方法

- 范围：nomic-core 领域模型
- 优先级：P1
- 发现时间：2026-10-18
- 现状：`ParentRef`、`ToolCall`、`ToolSpec`、`Submission`、`SubmissionRecord` 暴露公开字段；`Submission.content` 是 `String`，`Submission.working_dir` 是 `PathBuf`，`ToolSpec.description` 是 `String`。工具输入/输出类型（`ReadInput`、`BashInput`、`WriteOutput` 等）同样暴露公开字段。
- 问题：标准第 4 条要求 newtype 与充血模型。裸 `String` 与 `PathBuf` 无法表达「内容非空」「工作目录已解析」等不变量；公开字段让外部可以绕过构造函数制造非法状态。
- 期望结果：领域概念有专用 newtype；字段私有，读写经表达领域操作的方法。
- 建议方案：先明确协议与工具 schema 的纯数据传输类型是否豁免公开字段；再把领域类型逐个迁移到 newtype 与私有字段；迁移保持 serde 线上名不变。
- 验收标准：核心领域类型的字段不再 `pub`；既有 serde 往返测试通过；新增测试覆盖 newtype 的构造校验。
- 相关文件：crates/core/nomic-core/src/session.rs, crates/core/nomic-core/src/tool.rs, crates/core/nomic-core/src/submission.rs, crates/server/nomic-server/src/tools/read.rs

### T-005 为领域模式增加自动检查

- 范围：工具链
- 优先级：P2
- 发现时间：2026-10-18
- 现状：`STANDARD.md` 第 4 节（充血模型、newtype、typestate）标记为「本节无法自动检查，由评审保证」。`check` 目前只有 fmt、clippy、结构脚本、文件规模等检查。
- 问题：模式规则完全依赖评审；新增代码退化时检查不会失败。
- 期望结果：可判定的子集有自动检查，例如领域 crate 的公开字段、用于表达状态的布尔字段。
- 建议方案：评估 dylint 自定义 lint；若不可行，增加基于 Rust 语法树（`syn`）的扫描工具，并用它检查领域 crate。禁止用正则解析 Rust 源码。
- 验收标准：一个违反模式的样例被 `check` 拒绝；现有代码不产生误报。
- 相关文件：scripts/check_standards.py, Cargo.toml

### T-006 为 scripts/ 下的 Python 工具增加格式化与静态检查

- 范围：工具链
- 优先级：P2
- 发现时间：2026-10-18
- 现状：`scripts/check_standards.py` 是仓库唯一的 Python 文件；`check` 不包含 Python 的格式化与静态检查，devenv 只提供 `python3`。
- 问题：Python 工具的风格与正确性没有门禁，一致性依赖评审。
- 期望结果：Python 脚本与 Rust 代码受同等门禁约束。
- 建议方案：在 devenv 中加入 ruff，并在 `check` 中调用 `ruff format --check` 与 `ruff check`；配置放入 `ruff.toml`。
- 验收标准：`check` 包含两个 ruff 步骤；存在违规时检查失败。
- 相关文件：devenv.nix, scripts/check_standards.py
