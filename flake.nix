{
  description = "nomic — Rust 编码 agent（pi-coding-agent 的 Rust 复刻）";

  inputs = {
    # 与 devenv.lock 保持相同的固定 rev，保证 flake 构建与 devenv/CI 使用
    # 同一套 nixpkgs 与 rust-overlay（rust-toolchain.toml 固定 1.97.0，旧 rev
    # 的 rust-overlay 可能无法解析该版本）。
    nixpkgs.url = "github:cachix/devenv-nixpkgs/6004ea8c229fe9d41b21c6f4c76bf6c2e10771dd";
    rust-overlay = {
      url = "github:oxalica/rust-overlay/19a19f3921ae195f2fbd85f5dc57e6d1df63aa0b";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # crane：Rust 依赖与源码分层缓存构建，无需手工维护 cargoHash
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      crane,
      ...
    }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          # 单一事实来源：rust-toolchain.toml（devenv / CI / flake 共用）
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

          # cleanCargoSource 只保留 Cargo 相关文件，会误删 sqlx migrate! 宏
          # 编译期内嵌的 migrations/*.sql，需显式放行
          src = nixpkgs.lib.cleanSourceWith {
            src = ./.;
            filter =
              path: type:
              (builtins.match ".*\\.sql$" path != null) || (craneLib.filterCargoSources path type);
          };

          # web/dist 被 gitignore，flake 源码拷贝（仅含 git 跟踪文件）不含它，
          # 而 rust-embed 编译期需要内嵌，因此在沙箱内自行构建前端产物。
          # 过滤掉本地 node_modules/dist/storybook-static，避免污染求值。
          webSrc = nixpkgs.lib.cleanSourceWith {
            src = ./web;
            filter =
              path: type:
              let
                rel = nixpkgs.lib.removePrefix (toString ./web + "/") (toString path);
              in
              !(nixpkgs.lib.hasPrefix "node_modules" rel)
              && !(nixpkgs.lib.hasPrefix "dist" rel)
              && !(nixpkgs.lib.hasPrefix "storybook-static" rel);
          };
          webDist = pkgs.stdenv.mkDerivation {
            pname = "nomic-web";
            version = (builtins.fromJSON (builtins.readFile ./web/package.json)).version;
            src = webSrc;
            nativeBuildInputs = [
              pkgs.nodejs
              pkgs.importNpmLock.npmConfigHook
            ];
            npmDeps = pkgs.importNpmLock { npmRoot = ./web; };
            buildPhase = ''
              runHook preBuild
              npm run build
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              cp -r dist $out
              runHook postInstall
            '';
          };

          commonArgs = {
            inherit src;
            pname = "nomic";
            strictDeps = true;
            # sqlx 未使用编译期 query! 宏，无需 DATABASE_URL 等构建期环境
          };

          # 第一阶段：仅构建依赖（Cargo.lock 不变即可复用缓存）
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          # nomic 本体
          nomic = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              # rust-embed 编译期内嵌前端产物（沙箱内构建的 web/dist）
              preBuild = ''
                mkdir -p web
                ln -s ${webDist} web/dist
              '';
              # workspace 产物只需 nomic 二进制
              cargoExtraArgs = "--package nomic-cli";
              # nix 构建沙箱中 HOME（/homeless-shelter）不可写，而 nomic 缺省
              # 向平台标准 state 目录写滚动日志（Linux：XDG state，
              # fallback ~/.local/state）。
              # 检查阶段把 HOME 指到可写目录，否则 cli 集成测试 spawn 出的
              # 二进制初始化日志失败（EACCES）。
              preCheck = ''
                export HOME="$TMPDIR/nomic-home"
                mkdir -p "$HOME"
                # 沙箱内无系统 CA 证书，rustls 加载失败会导致二进制启动即 panic
                export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
              '';
              nativeCheckInputs = [ pkgs.cacert ];
              meta = {
                description = "Rust 编码 agent（pi-coding-agent 的 Rust 复刻）";
                homepage = "https://github.com/zineyu/nomic";
                license = nixpkgs.lib.licenses.mit;
                mainProgram = "nomic";
              };
            }
          );
        in
        {
          default = nomic;
          # 单独暴露便于调试/缓存：`nix build .#web`
          web = webDist;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/nomic";
        };
      });

      # `nix flake check`：保证包可构建（完整质量检查由 devenv `check` 脚本承担）
      checks = forAllSystems (system: {
        package = self.packages.${system}.default;
      });
    };
}
