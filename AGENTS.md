# AGENTS.md

> nomic 仓库当前为初始骨架状态：源码已清空，仅保留 CI 与 Rust 工具链配置。
> 新增 crate 放入 `crates/<category>/<name>/` 并加入 `Cargo.toml` 的 workspace members。

## Build & Check

一切操作在 devenv 环境内进行（不要在环境外安装或运行项目依赖）：

- 进入环境：`devenv shell`
- 提交前必须通过 `check`（与 CI 等价的完整检查：fmt / size / clippy / test / doctest / doc / deny / audit / machete / taplo / typos，定义在 `devenv.nix`）
- 常用单步：
  - Build: `cargo build --workspace --all-features --locked`
  - Format: `cargo fmt --all`
  - Lint: `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
  - Test: `cargo nextest run --workspace --all-features --locked`

## Code Style

- Formatter: `rustfmt`（`rustfmt.toml`）；Linter: `clippy`（`clippy.toml`，msrv 1.97）
- 严格 workspace lints（`Cargo.toml` `[workspace.lints]`）：`unsafe_code = "forbid"`，clippy `all` / `pedantic` / `nursery` / `cargo` 全部 deny
- TOML: `taplo fmt`（`taplo.toml`）；拼写: `typos`（白名单 `_typos.toml`，误报时把词加入其中）
- 单文件 800 行上限（`scripts/check-file-size.sh` 棘轮门禁）

## Testing

- Framework: `cargo-nextest`（单元 + 集成）
- 测试写在各 crate 内的 `#[cfg(test)]` 模块或 `tests/` 目录

## Security & Safety

- 禁止提交密钥、token、真实 `.env` 文件
- 全 workspace 禁止 `unsafe_code`
- 新增依赖前确认：许可证兼容（`cargo deny`）、无漏洞（`cargo audit`）、无未使用依赖（`cargo-machete`）；优先复用已有依赖，核心第三方调用用薄封装隔离

## Commit & Release

- 简洁的 commit subject，可用 Conventional Commit 前缀（`feat:`、`fix:`、`docs:` 等）
- 每个 commit 只做一件事、可独立 revert；版本管理用 Jujutsu（`jj`），推送 review 前整理为原子 commit
- 发版：`release <semver>`（devenv 脚本：bump 版本 → 生成 CHANGELOG → 跑完整 check → 创建 release 分支；PR 合并后 GitHub Actions 自动打 tag 并发布）
