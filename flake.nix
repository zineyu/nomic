{
  description = "nomic";

  inputs = {
    # 与 devenv.lock 保持相同的固定 rev，保证 flake 与 devenv/CI 使用
    # 同一套 nixpkgs 与 rust-overlay（rust-toolchain.toml 固定 1.97.0，旧 rev
    # 的 rust-overlay 可能无法解析该版本）。
    nixpkgs.url = "github:cachix/devenv-nixpkgs/6004ea8c229fe9d41b21c6f4c76bf6c2e10771dd";
    rust-overlay = {
      url = "github:oxalica/rust-overlay/19a19f3921ae195f2fbd85f5dc57e6d1df63aa0b";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
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
      # 开发壳：提供与 devenv / CI 完全一致的 Rust 工具链。
      # 完整开发环境请使用 devenv（见 devenv.nix）。
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
          };
          # 单一事实来源：rust-toolchain.toml（devenv / CI / flake 共用）
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            packages = [ toolchain ];
          };
        }
      );
    };
}
