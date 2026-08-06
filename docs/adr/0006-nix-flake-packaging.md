# ADR-0006: Nix flake 打包与安装方式

## Status

Accepted

## Date

2026-07-30

## Context

nomic 此前只能从源码 `cargo build` 构建。项目开发环境已由 devenv（nix）管理，
希望对外提供一等公民的 Nix 安装方式（`nix run` / `nix profile install`），
同时不把 Nix 强加为唯一安装途径（`cargo install` 仍然可用）。

需要决策的点：

- 构建器选择：`rustPlatform.buildRustPackage` 还是 crane；
- 工具链来源：如何避免 flake 与 devenv / CI 的 rustc 版本漂移；
- 与既有质量门禁（devenv `check` 脚本、CI）的关系。

## Decision

在仓库根目录新增 `flake.nix`，用 **crane** 两阶段构建：

1. **crane 而非 buildRustPackage**。crane 从 `Cargo.lock` 直接 vendor 依赖，
   无需手工维护 `cargoHash`（每次 lockfile 变动都要更新 hash 是
   buildRustPackage 的主要维护负担）；且依赖与源码分层缓存（`buildDepsOnly`
   + `buildPackage`），lockfile 不变时二次构建只编译本仓库代码。
2. **工具链单一事实来源仍是 `rust-toolchain.toml`**。flake 通过 rust-overlay
   的 `fromRustupToolchainFile` 读取，与 devenv、GitHub Actions 使用完全相同
   的 rustc 1.97.0。不使用 nixpkgs 默认 rustc。
3. **nixpkgs 与 rust-overlay 输入固定到与 `devenv.lock` 相同的 rev**
   （`cachix/devenv-nixpkgs@6004ea8c…`、`oxalica/rust-overlay@19a19f39…`），
   避免 flake 与开发环境的底层依赖漂移；升级时两者应同步推进。
4. **输出**：`packages.default`（nomic 二进制）、`apps.default`（`nix run`
   入口）、`checks.package`（`nix flake check` 构建包并跑 crane check 阶段
   的 cargo test）。完整质量门禁（fmt/clippy/deny/audit/…）仍唯一由 devenv
   `check` 脚本定义，flake 不复刻，避免两份检查定义漂移。
5. **CI 新增 `nix flake check` job**，防止 flake 腐烂；不引入 Cachix
   （用户首次安装需本地编译，见 Follow-ups）。

构建期的三个沙箱适配（均为环境差异，非代码缺陷）：

- sqlx `migrate!` 宏编译期内嵌 `migrations/*.sql`，而 crane 的
  `cleanCargoSource` 会将其过滤，需在 `src` filter 中显式放行 `.sql` 文件；
- 沙箱 `HOME` 不可写，nomic 缺省向 XDG state 目录写滚动日志，check 阶段把
  `HOME` 指向可写目录；
- 沙箱无系统 CA 证书，rustls 启动即 panic，check 阶段设置 `SSL_CERT_FILE`
  指向 `pkgs.cacert`。

## Consequences

- 用户可 `nix run github:zineyu/nomic` 直接运行，`nix profile install
  github:zineyu/nomic` 安装；`Cargo.lock` 变动无需任何 flake 侧维护。
- `flake.lock` 提交进仓库，保证可复现。
- 升级 rust 版本或 nixpkgs 时，需要同时更新 `rust-toolchain.toml`（已同步
  devenv/CI）与 flake 输入 rev（对照 `devenv.lock`）。

## Follow-ups

- 接入 Cachix 二进制缓存，使用户安装免编译；
- 发布 release 后可将 flake 输出与 GitHub Releases 二进制对应；
- 如需 Home Manager 模块或 overlay 输出，后续按需添加。
