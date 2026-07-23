# nomic

Rust workspace，使用 devenv 管理开发环境。

## 开发环境

```bash
devenv shell        # 进入开发 shell（工具链版本由 rust-toolchain.toml 固定）
direnv allow        # 或使用 direnv 自动进入
```

## 本地检查

```bash
check               # 与 CI 等价的全部检查：fmt / clippy / nextest / doc / deny / machete / taplo / typos
```

## 结构

- `crates/`：workspace 成员 crate
- `rust-toolchain.toml`：Rust 工具链单一事实来源（devenv 与 CI 共用）
- `Cargo.toml` `[workspace.lints]`：严格 lint 配置（deny `all`/`pedantic`/`nursery`/`cargo`，噪音 lint 显式豁免并注明理由）
- `deny.toml` / `_typos.toml` / `taplo.toml` / `clippy.toml` / `rustfmt.toml`：各工具配置
- `.github/workflows/ci.yml`：CI 流水线，分支保护勾选 `CI success` 一个检查即可

## 新增 crate

```bash
cargo new crates/<name> --lib
```

crate 的 `Cargo.toml` 中继承 workspace 配置：

```toml
[lints]
workspace = true
```
