{ pkgs, ... }:
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
    cargo-edit # cargo set-version：发版时 bump workspace 版本
    git-cliff # 从 conventional commits 生成 CHANGELOG
    python3 # 规范化 release 生成的 CHANGELOG 文件尾
    taplo # TOML 格式化与校验
    typos # 拼写检查
    ripgrep # 快速文本搜索（rg）
    fd # 快速文件查找

    # 常见原生依赖，按需取消注释：
    # pkg-config
    # openssl
  ];

  env.RUST_BACKTRACE = "1";

  # ── 本地一键检查（与 CI 等价）─────────────────────────────────────────────
  scripts.check.exec = ''
    set -e
    echo "== fmt =="       && cargo fmt --all -- --check
    echo "== size =="      && scripts/check-file-size.sh
    echo "== clippy =="    && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    echo "== test =="      && cargo nextest run --workspace --all-features --locked
    echo "== doctest =="   && cargo test --workspace --doc --locked
    echo "== doc =="       && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
    echo "== deny =="      && cargo deny check
    echo "== audit =="     && cargo audit
    echo "== machete =="   && cargo-machete --with-metadata
    echo "== taplo =="     && taplo fmt --check
    echo "== typos =="     && typos
  '';

  # ── 版本发布 ─────────────────────────────────────────────────────────────
  # 用法：release <semver>（不带 v 前缀），如 `release 0.2.0`
  # 流程：前置校验 → bump 版本 → git-cliff 生成 CHANGELOG → 完整 check →
  #       jj 提交并放到 release/vX.Y.Z 分支。推分支、建/合并 PR 由人工执行；
  #       PR 合并后 release-tag.yml 自动在 main 上打 tag 并派发 release.yml 发布。
  #       （main 受 ruleset 保护必须走 PR，不能本地直推 main + tag）
  scripts.release = {
    description = "发布新版本：release <semver>（不带 v 前缀），例如 release 0.2.0（生成 release 分支，PR 合并后自动发布）";
    exec = ''
      set -euo pipefail

      VERSION="''${1:-}"
      if [ -z "$VERSION" ]; then
        echo "用法: release <semver>（不带 v 前缀），例如: release 0.2.0" >&2
        exit 1
      fi
      TAG="v$VERSION"
      BRANCH="release/$TAG"

      if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
        echo "错误: '$VERSION' 不是合法的 semver 版本号" >&2
        exit 1
      fi

      if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
        echo "错误: tag $TAG 已存在" >&2
        exit 1
      fi

      # 工作副本必须是空 @，避免把无关改动带进 release commit
      if [ "$(jj log -r @ --no-graph -T 'if(empty, "true", "false")')" != "true" ]; then
        echo "错误: 工作副本有未提交改动，请先 jj commit / jj squash 整理" >&2
        exit 1
      fi

      # 必须基于 main 最新提交发版
      if [ "$(jj log -r main --no-graph -T commit_id)" != "$(jj log -r @- --no-graph -T commit_id)" ]; then
        echo "错误: 请先切换到 main 最新提交（jj new main）再发版" >&2
        exit 1
      fi

      echo "== bump: $TAG =="
      cargo set-version --workspace "$VERSION"
      # 同步 Cargo.lock 中的 workspace 版本（cargo metadata 会重写 lock）
      cargo metadata --format-version 1 > /dev/null

      echo "== changelog =="
      git-cliff --tag "$TAG" -o CHANGELOG.md
      # git-cliff 的文件尾换行数量不稳定；先规范化为恰好一个换行，避免
      # end-of-file-fixer 在后续 check 中修改文件并导致 release 中断。
      python3 - <<'PY'
      from pathlib import Path

      path = Path("CHANGELOG.md")
      path.write_bytes(path.read_bytes().rstrip(b"\r\n") + b"\n")
      PY

      # release commit 必须通过完整 check（RELEASE_SKIP_CHECK=1 可跳过，不推荐）
      if [ "''${RELEASE_SKIP_CHECK:-0}" != "1" ]; then
        echo "== check =="
        check
      fi

      echo "== commit =="
      jj commit -m "chore(release): $TAG"
      jj bookmark set "$BRANCH" -r @-

      echo ""
      echo "✅ release 分支已就绪（main 受保护，必须走 PR）。后续步骤："
      echo "   1. 推送分支并创建 PR（人工确认后执行）："
      echo "      jj git push --bookmark $BRANCH"
      echo "      gh pr create --base main --head $BRANCH \\"
      echo "        --title 'chore(release): $TAG' \\"
      echo "        --body '发布 $TAG。合并后 release-tag.yml 自动打 tag 并派发 release.yml 完成发布。'"
      echo "   2. PR 检查全绿后合并（merge/squash/rebase 均可）"
      echo "   3. 合并后自动进行：打 tag $TAG → 派发 release.yml → 门禁/构建/发布"
    '';
  };

  # ── Git hooks（提交前快速检查，重型检查留给 CI）───────────────────────────
  # 运行器默认为 prek（devenv 2026-02-02 起替代 pre-commit，配置格式兼容）。
  git-hooks.hooks = {
    # devenv 会把 languages.rust.toolchainPackage（由 rust-toolchain.toml 固定）
    # 注入 git-hooks.tools.rustfmt/cargo，hook 自动使用同一工具链，无需手动覆盖。
    rustfmt.enable = true;
    taplo.enable = true;
    typos.enable = true;
    check-toml.enable = true;
    trim-trailing-whitespace.enable = true;
    end-of-file-fixer.enable = true;
  };

  # ── devenv test：与 CI 等价的完整检查 ───────────────────────────────────
  enterTest = ''
    check
  '';

  enterShell = ''
    echo "🦀 nomic dev shell"
    echo "  rustc: $(rustc --version)"
    echo "  cargo: $(cargo --version)"
    echo "  运行 \`check\` 执行与 CI 等价的全部本地检查"
  '';
}
