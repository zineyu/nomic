{
  pkgs,
  config,
  ...
}:
{
  # ── Rust 工具链 ────────────────────────────────────────────────────────────
  languages.rust = {
    enable = true;
    # 版本由 rust-toolchain.toml 固定，devenv 通过 rust-overlay 的
    # fromRustupToolchainFile 读取，保证与 CI 使用完全相同的工具链。
    toolchainFile = ./rust-toolchain.toml;
    # 使用 mold 链接器加速本地构建
    mold.enable = pkgs.stdenv.isLinux;
    lsp.enable = true;
  };

  # ── 质量工具 ───────────────────────────────────────────────────────────────
  packages = with pkgs; [
    cargo-deny # 依赖许可证 / 安全公告 / 重复依赖检查
    cargo-audit # RustSec 漏洞审计
    cargo-machete # 未使用依赖检测
    cargo-nextest # 更快的测试运行器
    taplo # TOML 格式化与校验
    typos # 拼写检查

    # 常见原生依赖，按需取消注释：
    # pkg-config
    # openssl
  ];

  env.RUST_BACKTRACE = "1";

  # ── 本地一键检查（与 CI 等价）─────────────────────────────────────────────
  scripts.check.exec = ''
    set -e
    echo "== fmt =="       && cargo fmt --all -- --check
    echo "== clippy =="    && cargo clippy --workspace --all-targets --all-features -- -D warnings
    echo "== test =="      && cargo nextest run --workspace --all-features
    echo "== doc =="       && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    echo "== deny =="      && cargo deny check
    echo "== machete =="   && cargo machete --with-metadata
    echo "== taplo =="     && taplo fmt --check
    echo "== typos =="     && typos
  '';

  # ── Git hooks（提交前快速检查，重型检查留给 CI）───────────────────────────
  git-hooks.hooks = {
    rustfmt = {
      enable = true;
      # 与 rust-toolchain.toml 保持一致的格式化工具
      package = config.languages.rust.toolchainPackage;
    };
    taplo.enable = true;
    typos.enable = true;
    check-toml.enable = true;
    trim-trailing-whitespace.enable = true;
    end-of-file-fixer.enable = true;
  };

  enterShell = ''
    echo "🦀 nomic dev shell"
    echo "  rustc: $(rustc --version)"
    echo "  cargo: $(cargo --version)"
    echo "  运行 \`check\` 执行与 CI 等价的全部本地检查"
  '';
}
